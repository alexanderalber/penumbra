//! The JSON exchanged with the bootstrap inside Python (`crates/pyhost/src/bootstrap.py`), and
//! its conversion from and to penumbra's types. Pure Rust, tested without a Python.
//!
//! Pictures and meshes travel as raw arrays beside the JSON, not inside it: a request carries
//! one block of bytes the JSON points into (input pictures), a reply is a [`Frame`] of the JSON
//! and binary parts the JSON names by index. Everything in a reply is checked here again
//! (sizes, lengths, finite positions, indices in range), whatever the bootstrap checked before.

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::Path;

use penumbra_core::{Channel, Chans, Name, ParamSpec, Table, Value};
use serde::{Deserialize, Serialize};

/// Most pixels of a picture a script puts out or is handed: 4K UHD (8.3 million) fits.
pub const MAX_PICTURE_PIXELS: u64 = 1 << 23;
/// Longest side of such a picture.
pub const MAX_PICTURE_SIDE: u32 = 8192;
/// Most vertices of a mesh a script puts out.
pub const MAX_MESH_VERTICES: usize = 1 << 20;
/// Most triangles of a mesh a script puts out.
pub const MAX_MESH_TRIANGLES: usize = 1 << 21;

/// A picture handed to a script (`cx.pictures[i]`): RGBA as 32-bit floats, rows top to bottom.
#[derive(Clone, Debug, PartialEq)]
pub struct InPicture {
    pub width: u32,
    pub height: u32,
    /// Changes with every new picture. The bootstrap keeps the array of the last generation it
    /// was sent per node and input, so an unchanged picture crosses only once.
    pub generation: u64,
    /// `width * height * 4` samples as little-endian float32 bytes.
    pub bytes: Vec<u8>,
}

/// An input picture of a call, and whether its bytes go along (`false`: the bootstrap has
/// this generation already).
#[derive(Clone, Copy, Debug)]
pub struct PictureArg<'a> {
    pub picture: &'a InPicture,
    pub send: bool,
}

/// The samples of a picture a script put out, as the array held them.
#[derive(Clone, Debug, PartialEq)]
pub enum Samples {
    U8(Vec<u8>),
    /// Little-endian float32 bytes.
    F32(Vec<u8>),
}

/// The picture a script put out (`cx.picture`), checked: `width * height * channels` samples,
/// rows top to bottom.
#[derive(Clone, Debug, PartialEq)]
pub struct OutPicture {
    pub width: u32,
    pub height: u32,
    /// 1 (grey), 3 (RGB) or 4 (RGBA).
    pub channels: u8,
    pub samples: Samples,
}

/// The mesh a script put out (`cx.mesh`), checked: finite positions, indices inside them, the
/// optional arrays one entry per vertex.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OutMesh {
    pub positions: Vec<[f32; 3]>,
    /// `None`: every three vertices are a triangle.
    pub indices: Option<Vec<[u32; 3]>>,
    pub normals: Option<Vec<[f32; 3]>>,
    pub uvs: Option<Vec<[f32; 2]>>,
}

/// Loading a script into the module slot `key` (replacing what was there only if it loads).
#[derive(Clone, Debug)]
pub struct Load<'a> {
    pub key: u64,
    /// Increases with every load of a slot: a load older than the one in the slot is refused,
    /// so two loads racing each other cannot leave the older script running.
    pub generation: u64,
    pub source: &'a str,
    /// The name errors and tracebacks use (the file's name, or the node's).
    pub label: &'a str,
    /// Put on `sys.path`, so a script can import modules next to it.
    pub folder: Option<&'a Path>,
}

/// What a script declared when it loaded.
#[derive(Clone, Debug, PartialEq)]
pub struct Loaded {
    /// From its `PARAMS`.
    pub params: Vec<ParamSpec>,
    /// Names `time` or `frame` somewhere: recook every frame.
    pub uses_time: bool,
    pub printed: String,
}

/// One call of a loaded script's `cook(cx)`.
#[derive(Clone, Debug)]
pub struct Call<'a> {
    pub key: u64,
    /// Show time in seconds and frame number; `None` for a script that does not read them.
    pub time: Option<(f64, i64)>,
    /// Show frames per second, also the default rate of the output channels.
    pub rate: f64,
    pub params: &'a BTreeMap<String, Value>,
    pub chans: Vec<Option<&'a Chans>>,
    pub tables: Vec<Option<&'a Table>>,
    pub pictures: Vec<Option<PictureArg<'a>>>,
    /// Whether anything reads the picture and the mesh: the bootstrap leaves out what nobody
    /// reads.
    pub want_picture: bool,
    pub want_mesh: bool,
}

/// What a call produced.
#[derive(Clone, Debug, PartialEq)]
pub struct CallOutput {
    pub chans: Chans,
    pub table: Table,
    /// `None` when the script set none (or nothing reads it).
    pub picture: Option<OutPicture>,
    pub mesh: Option<OutMesh>,
    pub warnings: Vec<String>,
    pub printed: String,
}

/// A reply as pyhost sends it: little-endian `u64`s for the JSON's length, the number `n` of
/// binary parts and their `n` lengths, then the JSON and the parts.
#[derive(Debug)]
pub struct Frame {
    bytes: Vec<u8>,
    json: Range<usize>,
    parts: Vec<Range<usize>>,
}

impl Frame {
    pub fn parse(bytes: Vec<u8>) -> Result<Frame, String> {
        let bad = || "unreadable reply from Python (a broken frame)".to_string();
        let word = |i: usize| -> Result<usize, String> {
            let at = i.checked_mul(8).ok_or_else(bad)?;
            let b = bytes.get(at..at + 8).ok_or_else(bad)?;
            let mut w = [0u8; 8];
            w.copy_from_slice(b);
            usize::try_from(u64::from_le_bytes(w)).map_err(|_| bad())
        };
        let json_len = word(0)?;
        let n = word(1)?;
        // Each part needs its 8-byte length in the header: more parts than bytes is broken.
        if n > bytes.len() / 8 {
            return Err(bad());
        }
        let mut at = (2 + n) * 8;
        let take = |at: &mut usize, len: usize| -> Result<Range<usize>, String> {
            let end = at.checked_add(len).filter(|e| *e <= bytes.len()).ok_or_else(bad)?;
            let r = *at..end;
            *at = end;
            Ok(r)
        };
        let json = take(&mut at, json_len)?;
        let mut parts = Vec::with_capacity(n);
        for i in 0..n {
            let len = word(2 + i)?;
            parts.push(take(&mut at, len)?);
        }
        if at != bytes.len() {
            return Err(bad());
        }
        std::str::from_utf8(bytes.get(json.clone()).unwrap_or_default()).map_err(|_| bad())?;
        Ok(Frame { bytes, json, parts })
    }

    /// The JSON text (checked to be UTF-8 by [`Frame::parse`]).
    pub fn json(&self) -> &str {
        self.bytes.get(self.json.clone()).and_then(|b| std::str::from_utf8(b).ok()).unwrap_or("")
    }

    pub fn part(&self, i: usize) -> Option<&[u8]> {
        self.parts.get(i).and_then(|r| self.bytes.get(r.clone()))
    }

    /// A frame of `json` and `parts`, as pyhost builds it (for tests).
    pub fn build(json: &str, parts: &[&[u8]]) -> Vec<u8> {
        let lengths = [json.len(), parts.len()].into_iter().chain(parts.iter().map(|p| p.len()));
        let mut out: Vec<u8> = lengths.flat_map(|n| (n as u64).to_le_bytes()).collect();
        out.extend_from_slice(json.as_bytes());
        for p in parts {
            out.extend_from_slice(p);
        }
        out
    }
}

#[derive(Serialize)]
struct LoadJson<'a> {
    op: &'static str,
    key: u64,
    generation: u64,
    source: &'a str,
    file: &'a str,
    folder: Option<String>,
}

#[derive(Serialize)]
struct CallJson<'a> {
    op: &'static str,
    key: u64,
    time: Option<f64>,
    frame: Option<i64>,
    rate: f64,
    params: &'a BTreeMap<String, Value>,
    chans: Vec<Option<ChansJson<'a>>>,
    tables: Vec<Option<&'a [Vec<String>]>>,
    /// Where each input picture lies in the request's binary block.
    pictures: Vec<Option<PictureInJson>>,
    want_picture: bool,
    want_mesh: bool,
}

#[derive(Serialize)]
struct PictureInJson {
    width: u32,
    height: u32,
    generation: u64,
    /// Byte offset into the block of `width * height * 4` float32 samples; `None` when the
    /// bootstrap has this generation already.
    offset: Option<usize>,
}

#[derive(Serialize)]
struct ChansJson<'a> {
    rate: f64,
    names: Vec<&'a str>,
    samples: Vec<&'a [f64]>,
}

/// Every reply has `ok`; the rest depends on the request.
#[derive(Deserialize, Default)]
#[serde(default)]
pub(crate) struct Reply {
    pub ok: bool,
    pub error: String,
    pub timeout: bool,
    /// A call named an input picture by generation that the bootstrap does not hold: send it
    /// again with its bytes.
    pub resend: bool,
    pub printed: String,
    // load
    params: Vec<ParamJson>,
    uses_time: bool,
    // call
    names: Vec<String>,
    samples: Vec<Vec<f64>>,
    rate: Option<f64>,
    table: Vec<Vec<String>>,
    warnings: Vec<String>,
    picture: Option<PictureJson>,
    mesh: Option<MeshJson>,
}

/// `cx.picture` as the bootstrap describes it; the samples are binary part `part`.
#[derive(Deserialize)]
struct PictureJson {
    width: u32,
    height: u32,
    channels: u8,
    /// `u8` or `f32`.
    dtype: String,
    part: usize,
}

/// `cx.mesh`: the number of vertices, and which binary part holds each array (float32
/// positions, uint32 indices, float32 normals and uvs).
#[derive(Deserialize)]
struct MeshJson {
    vertices: usize,
    positions: usize,
    indices: Option<usize>,
    normals: Option<usize>,
    uvs: Option<usize>,
}

#[derive(Deserialize)]
struct ParamJson {
    name: String,
    kind: String,
    default: serde_json::Value,
    min: Option<f64>,
    max: Option<f64>,
    #[serde(default)]
    choices: Vec<String>,
}

pub(crate) fn load_json(load: &Load<'_>) -> Result<String, String> {
    serde_json::to_string(&LoadJson {
        op: "load",
        key: load.key,
        generation: load.generation,
        source: load.source,
        file: load.label,
        folder: load.folder.map(|f| f.display().to_string()),
    })
    .map_err(|e| e.to_string())
}

/// The request's JSON, and the binary block holding the input pictures.
pub(crate) fn call_request(call: &Call<'_>) -> Result<(String, Vec<u8>), String> {
    let total: usize = call.pictures.iter().flatten().filter(|a| a.send).map(|a| a.picture.bytes.len()).sum();
    let mut blob = Vec::with_capacity(total);
    let mut pictures = Vec::with_capacity(call.pictures.len());
    for a in &call.pictures {
        pictures.push(match a {
            Some(PictureArg { picture: p, send }) => {
                let expected = u64::from(p.width) * u64::from(p.height) * 16;
                if p.bytes.len() as u64 != expected {
                    return Err(format!(
                        "an input picture of {} x {} has {} bytes",
                        p.width,
                        p.height,
                        p.bytes.len()
                    ));
                }
                let offset = send.then_some(blob.len());
                if *send {
                    blob.extend_from_slice(&p.bytes);
                }
                Some(PictureInJson { width: p.width, height: p.height, generation: p.generation, offset })
            }
            None => None,
        });
    }
    let chans = call
        .chans
        .iter()
        .map(|c| {
            c.map(|c| ChansJson {
                rate: c.rate(),
                names: c.channels().iter().map(|ch| ch.name.as_str()).collect(),
                samples: c.channels().iter().map(|ch| ch.samples.as_slice()).collect(),
            })
        })
        .collect();
    serde_json::to_string(&CallJson {
        op: "call",
        key: call.key,
        time: call.time.map(|t| t.0),
        frame: call.time.map(|t| t.1),
        rate: call.rate,
        params: call.params,
        chans,
        tables: call.tables.iter().map(|t| t.map(Table::rows)).collect(),
        pictures,
        want_picture: call.want_picture,
        want_mesh: call.want_mesh,
    })
    .map(|text| (text, blob))
    .map_err(|e| e.to_string())
}

pub(crate) fn parse(text: &str) -> Result<Reply, String> {
    serde_json::from_str(text).map_err(|e| format!("unreadable reply from Python: {e}"))
}

impl Reply {
    pub(crate) fn loaded(self) -> Result<Loaded, String> {
        let params = self.params.into_iter().map(param_spec).collect::<Result<_, _>>()?;
        Ok(Loaded { params, uses_time: self.uses_time, printed: self.printed })
    }

    pub(crate) fn output(self, show_rate: f64, frame: &Frame) -> Result<CallOutput, String> {
        let picture = self.picture.map(|p| picture(p, frame)).transpose()?;
        let mesh = self.mesh.map(|m| mesh(m, frame)).transpose()?;
        let channels = self
            .names
            .into_iter()
            .zip(self.samples)
            .map(|(name, samples)| {
                let name = Name::new(&name).map_err(|e| format!("cx.out[{name:?}]: {e}"))?;
                Ok(Channel { name, samples })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let chans =
            Chans::new(self.rate.unwrap_or(show_rate), channels).map_err(|e| format!("cx.out: {e}"))?;
        let table = Table::new(self.table).map_err(|e| format!("cx.table: {e}"))?;
        Ok(CallOutput { chans, table, picture, mesh, warnings: self.warnings, printed: self.printed })
    }
}

fn picture(p: PictureJson, frame: &Frame) -> Result<OutPicture, String> {
    let (w, h) = (p.width, p.height);
    if w == 0 || h == 0 {
        return Err(format!("cx.picture: {w} x {h} pixels is empty"));
    }
    let pixels = u64::from(w) * u64::from(h);
    if w > MAX_PICTURE_SIDE || h > MAX_PICTURE_SIDE || pixels > MAX_PICTURE_PIXELS {
        return Err(format!(
            "cx.picture: {w} x {h} pixels is too large (at most {MAX_PICTURE_SIDE} on a side and {MAX_PICTURE_PIXELS} pixels)"
        ));
    }
    if !matches!(p.channels, 1 | 3 | 4) {
        return Err(format!("cx.picture: {} channels; give 1, 3 or 4", p.channels));
    }
    let bytes = frame.part(p.part).ok_or("cx.picture: the samples are missing")?;
    let count = pixels * u64::from(p.channels);
    let samples = match p.dtype.as_str() {
        "u8" if bytes.len() as u64 == count => Samples::U8(bytes.to_vec()),
        "f32" if bytes.len() as u64 == count * 4 => Samples::F32(bytes.to_vec()),
        "u8" | "f32" => {
            return Err(format!("cx.picture: {} bytes for {w} x {h} x {}", bytes.len(), p.channels));
        }
        other => return Err(format!("cx.picture: samples of type {other}; give uint8 or float32")),
    };
    Ok(OutPicture { width: w, height: h, channels: p.channels, samples })
}

fn floats(bytes: &[u8]) -> Vec<f32> {
    bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()
}

/// Binary part `part` as `n` rows of `K` float32s.
fn rows<const K: usize>(frame: &Frame, part: usize, n: usize, what: &str) -> Result<Vec<[f32; K]>, String> {
    let bytes = frame.part(part).ok_or_else(|| format!("cx.mesh[{what:?}]: the array is missing"))?;
    if bytes.len() != n * K * 4 {
        return Err(format!("cx.mesh[{what:?}]: {} bytes for {n} rows of {K} numbers", bytes.len()));
    }
    let values = floats(bytes);
    Ok(values.as_chunks::<K>().0.to_vec())
}

fn mesh(m: MeshJson, frame: &Frame) -> Result<OutMesh, String> {
    let n = m.vertices;
    if n == 0 {
        return Err("cx.mesh: no vertices".into());
    }
    if n > MAX_MESH_VERTICES {
        return Err(format!("cx.mesh: {n} vertices is too many (at most {MAX_MESH_VERTICES})"));
    }
    let positions = rows::<3>(frame, m.positions, n, "positions")?;
    if positions.iter().flatten().any(|v| !v.is_finite()) {
        return Err("cx.mesh[\"positions\"]: a position is NaN or infinite".into());
    }
    let indices = match m.indices {
        Some(part) => {
            let bytes = frame.part(part).ok_or("cx.mesh[\"indices\"]: the array is missing")?;
            if bytes.len() % 12 != 0 {
                return Err(format!("cx.mesh[\"indices\"]: {} bytes is not whole triangles", bytes.len()));
            }
            if bytes.len() / 12 > MAX_MESH_TRIANGLES {
                return Err(format!(
                    "cx.mesh: {} triangles is too many (at most {MAX_MESH_TRIANGLES})",
                    bytes.len() / 12
                ));
            }
            let words: Vec<u32> = bytes.as_chunks::<4>().0.iter().map(|b| u32::from_le_bytes(*b)).collect();
            let tris = words.as_chunks::<3>().0.to_vec();
            if tris.is_empty() {
                return Err("cx.mesh[\"indices\"]: no triangles".into());
            }
            if tris.len() > MAX_MESH_TRIANGLES {
                return Err(format!(
                    "cx.mesh: {} triangles is too many (at most {MAX_MESH_TRIANGLES})",
                    tris.len()
                ));
            }
            if let Some(i) = tris.iter().flatten().find(|i| **i as usize >= n) {
                return Err(format!("cx.mesh[\"indices\"]: index {i} is past the last vertex ({})", n - 1));
            }
            Some(tris)
        }
        None if !n.is_multiple_of(3) => {
            return Err(format!(
                "cx.mesh: {n} vertices without indices; every three vertices make a triangle, so give a multiple of 3"
            ));
        }
        None => None,
    };
    let normals = m.normals.map(|p| rows::<3>(frame, p, n, "normals")).transpose()?;
    let uvs = m.uvs.map(|p| rows::<2>(frame, p, n, "uvs")).transpose()?;
    Ok(OutMesh { positions, indices, normals, uvs })
}

fn param_spec(p: ParamJson) -> Result<ParamSpec, String> {
    let ctx = |e: String| format!("PARAMS[{:?}]: {e}", p.name);
    let range = |spec: ParamSpec| match (p.min, p.max) {
        (Some(min), Some(max)) => spec.range(min, max),
        _ => spec,
    };
    let spec = match (p.kind.as_str(), &p.default) {
        ("bool", serde_json::Value::Bool(b)) => ParamSpec::bool(&p.name, *b),
        ("int", d) if d.as_i64().is_some() => range(ParamSpec::int(&p.name, d.as_i64().unwrap_or(0))),
        ("float", d) if d.as_f64().is_some() => range(ParamSpec::float(&p.name, d.as_f64().unwrap_or(0.0))),
        ("text", serde_json::Value::String(s)) => ParamSpec::text(&p.name, s),
        ("choice", _) => {
            let choices: Vec<&str> = p.choices.iter().map(String::as_str).collect();
            ParamSpec::choice(&p.name, &choices)
        }
        (kind, d) => return Err(ctx(format!("cannot declare a {kind} with default {d}"))),
    };
    spec.validate_spec().map_err(|e| ctx(e.0))?;
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_json_carries_inputs() {
        let chans =
            Chans::new(30.0, vec![Channel { name: Name::new("x").unwrap(), samples: vec![1.0, 2.5] }])
                .unwrap();
        let table = Table::new(vec![vec!["a".into(), "b".into()]]).unwrap();
        let params = BTreeMap::from([("gain".to_string(), Value::Float(0.5))]);
        let rgba = [0.0f32, 0.25, 0.5, 1.0, 1.0, 1.0, 1.0, 1.0];
        let picture = InPicture {
            width: 2,
            height: 1,
            generation: 5,
            bytes: rgba.iter().flat_map(|v| v.to_le_bytes()).collect(),
        };
        let old = InPicture { generation: 4, ..picture.clone() };
        let (text, blob) = call_request(&Call {
            key: 7,
            time: Some((1.5, 90)),
            rate: 60.0,
            params: &params,
            chans: vec![Some(&chans), None],
            tables: vec![None, Some(&table)],
            pictures: vec![
                Some(PictureArg { picture: &old, send: false }),
                Some(PictureArg { picture: &picture, send: true }),
            ],
            want_picture: true,
            want_mesh: false,
        })
        .unwrap();
        assert_eq!(blob.len(), 32);
        assert_eq!(blob[4..8], 0.25f32.to_le_bytes());
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(v["pictures"][0]["offset"].is_null());
        assert_eq!(v["pictures"][0]["generation"].as_u64(), Some(4));
        assert_eq!(
            (v["pictures"][1]["width"].as_u64(), v["pictures"][1]["offset"].as_u64()),
            (Some(2), Some(0))
        );
        assert_eq!((v["want_picture"].as_bool(), v["want_mesh"].as_bool()), (Some(true), Some(false)));
        assert_eq!(v["op"], "call");
        assert_eq!(v["chans"][0]["names"][0], "x");
        assert_eq!(v["chans"][0]["samples"][0][1], 2.5);
        assert!(v["chans"][1].is_null());
        assert_eq!(v["tables"][1][0][1], "b");
        assert_eq!(v["params"]["gain"], 0.5);
        assert_eq!((v["time"].as_f64(), v["frame"].as_i64()), (Some(1.5), Some(90)));
    }

    #[test]
    fn replies_become_chans_tables_and_params() {
        let r = parse(
            r#"{"ok": true, "names": ["x", "y"], "samples": [[1, 2], [3, 4]], "rate": null,
            "table": [["a"], []], "warnings": ["w"], "printed": "hi\n"}"#,
        )
        .unwrap();
        let none = Frame::parse(Frame::build("{}", &[])).unwrap();
        let out = r.output(60.0, &none).unwrap();
        assert_eq!(out.chans.rate(), 60.0);
        assert_eq!(out.chans.value(Some("y")), Some(4.0));
        assert_eq!(out.table.num_rows(), 2);
        assert_eq!(out.warnings, ["w"]);
        assert_eq!((out.picture, out.mesh), (None, None));

        let bad = parse(r#"{"ok": true, "names": ["no space"], "samples": [[1]]}"#).unwrap();
        assert!(bad.output(60.0, &none).unwrap_err().starts_with("cx.out[\"no space\"]"));

        let r = parse(
            r#"{"ok": true, "uses_time": true, "params": [
            {"name": "gain", "kind": "float", "default": 1.0, "min": 0.0, "max": 2.0},
            {"name": "count", "kind": "int", "default": 3},
            {"name": "on", "kind": "bool", "default": false},
            {"name": "label", "kind": "text", "default": "hi"},
            {"name": "mode", "kind": "choice", "default": "a", "choices": ["a", "b"]}]}"#,
        )
        .unwrap();
        let l = r.loaded().unwrap();
        assert!(l.uses_time);
        assert_eq!(l.params.len(), 5);
        assert_eq!(l.params[0].default, Value::Float(1.0));
        let r =
            parse(r#"{"ok": true, "params": [{"name": "bad name", "kind": "int", "default": 1}]}"#).unwrap();
        assert!(r.loaded().unwrap_err().contains("PARAMS[\"bad name\"]"));
    }

    #[test]
    fn frames_are_checked() {
        let f = Frame::parse(Frame::build("{\"ok\": true}", &[b"abc", b""])).unwrap();
        assert_eq!(f.json(), "{\"ok\": true}");
        assert_eq!((f.part(0), f.part(1), f.part(2)), (Some(&b"abc"[..]), Some(&b""[..]), None));
        let good = Frame::build("{}", &[b"abc"]);
        for broken in [&good[..good.len() - 1], &good[..4], &[][..]] {
            assert!(Frame::parse(broken.to_vec()).is_err());
        }
        let mut longer = good.clone();
        longer.push(0);
        assert!(Frame::parse(longer).is_err(), "bytes after the last part");
        let mut huge = good;
        huge[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(Frame::parse(huge).is_err(), "a part count past the frame");
        assert!(Frame::parse(Frame::build("\u{0}", &[])).is_ok());
        let mut not_utf8 = Frame::build("ab", &[]);
        let at = not_utf8.len() - 1;
        not_utf8[at] = 0xff;
        assert!(Frame::parse(not_utf8).is_err());
    }

    fn reply_with(json: &str, parts: &[&[u8]]) -> Result<CallOutput, String> {
        let frame = Frame::parse(Frame::build(json, parts)).unwrap();
        parse(frame.json()).unwrap().output(60.0, &frame)
    }

    fn f32s(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    fn u32s(v: &[u32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    #[test]
    fn pictures_are_checked() {
        let pic = |w: u32, h: u32, c: u8, dtype: &str| {
            format!(
                r#"{{"ok": true, "picture": {{"width": {w}, "height": {h}, "channels": {c}, "dtype": "{dtype}", "part": 0}}}}"#
            )
        };
        let out = reply_with(&pic(2, 1, 3, "u8"), &[&[1, 2, 3, 4, 5, 6]]).unwrap();
        assert_eq!(
            out.picture,
            Some(OutPicture {
                width: 2,
                height: 1,
                channels: 3,
                samples: Samples::U8(vec![1, 2, 3, 4, 5, 6])
            })
        );
        let out = reply_with(&pic(1, 1, 1, "f32"), &[&f32s(&[0.5])]).unwrap();
        assert_eq!(out.picture.unwrap().samples, Samples::F32(f32s(&[0.5])));
        let err = |json: &str, parts: &[&[u8]]| reply_with(json, parts).unwrap_err();
        assert!(err(&pic(2, 1, 3, "u8"), &[&[1, 2, 3]]).contains("3 bytes for 2 x 1 x 3"));
        assert!(err(&pic(1, 1, 2, "u8"), &[&[1, 2]]).contains("give 1, 3 or 4"));
        assert!(err(&pic(1, 1, 1, "f64"), &[&[0; 8]]).contains("give uint8 or float32"));
        assert!(err(&pic(0, 1, 1, "u8"), &[&[]]).contains("empty"));
        assert!(err(&pic(8193, 1, 1, "u8"), &[&[]]).contains("too large"));
        assert!(err(&pic(4096, 4096, 1, "u8"), &[&[]]).contains("too large"));
        assert!(err(&pic(1, 1, 1, "u8"), &[]).contains("missing"));
    }

    #[test]
    fn meshes_are_checked() {
        let mesh = |n: usize, extra: &str| {
            format!(r#"{{"ok": true, "mesh": {{"vertices": {n}, "positions": 0{extra}}}}}"#)
        };
        let tri = f32s(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
        let out = reply_with(&mesh(3, ""), &[&tri]).unwrap();
        let m = out.mesh.unwrap();
        assert_eq!(m.positions[1], [1.0, 0.0, 0.0]);
        assert_eq!((m.indices, m.normals, m.uvs), (None, None, None));
        let idx = u32s(&[0, 1, 2, 2, 1, 0]);
        let uv = f32s(&[0.0; 6]);
        let nrm = f32s(&[0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0]);
        let full = mesh(3, r#", "indices": 1, "normals": 2, "uvs": 3"#);
        let m = reply_with(&full, &[&tri, &idx, &nrm, &uv]).unwrap().mesh.unwrap();
        assert_eq!(m.indices.unwrap(), [[0, 1, 2], [2, 1, 0]]);
        assert_eq!(m.normals.unwrap()[2], [0.0, 0.0, 1.0]);
        assert_eq!(m.uvs.unwrap().len(), 3);

        let err = |json: &str, parts: &[&[u8]]| reply_with(json, parts).unwrap_err();
        let nan = f32s(&[f32::NAN, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
        assert!(err(&mesh(3, ""), &[&nan]).contains("NaN"));
        assert!(err(&mesh(2, ""), &[&tri[..24]]).contains("multiple of 3"));
        assert!(err(&mesh(3, ""), &[&tri[..24]]).contains("bytes for 3 rows"));
        assert!(err(&mesh(0, ""), &[&[]]).contains("no vertices"));
        assert!(err(&mesh(MAX_MESH_VERTICES + 1, ""), &[&[]]).contains("too many"));
        let far = u32s(&[0, 1, 3]);
        assert!(
            err(&mesh(3, r#", "indices": 1"#), &[&tri, &far]).contains("index 3 is past the last vertex (2)")
        );
        assert!(err(&mesh(3, r#", "indices": 1"#), &[&tri, &idx[..8]]).contains("not whole triangles"));
        assert!(err(&mesh(3, r#", "indices": 1"#), &[&tri, &[]]).contains("no triangles"));
        assert!(err(&mesh(3, r#", "uvs": 1"#), &[&tri, &uv[..8]]).contains("cx.mesh[\"uvs\"]"));
        assert!(err(&mesh(3, r#", "normals": 5"#), &[&tri]).contains("missing"));
    }
}
