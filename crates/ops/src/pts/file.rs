//! `pts.file`: a point cloud from a file (TouchDesigner's Point File In POP): `.ply` (ASCII or
//! binary little endian) with position, normal, colour and any other number per vertex, and
//! `.xyz`, `.csv` or `.txt` with a number per column.
//!
//! The file is read and parsed off the cook thread, again when it is saved. Every size in a
//! file is a claim: the count a header gives is capped by [`MAX_POINTS`] and by what the file
//! really holds, so a short, cut or made-up file gives the points it has (with a warning) and
//! never more.

use std::path::Path;

use penumbra_core::{Error, ParamSpec};
use penumbra_gpu::Points;
use penumbra_gpu::points::{AttrType, MAX_ATTRS, valid_name};
use penumbra_graph::{
    AsyncSlot, CookContext, CookError, Cooked, Data, DataKind, FileWatch, FnNodeType, JobStatus, NodeDesc,
    NodeTypes, Operator,
};

use super::from_values::columns;
use super::upload::{Column, Uploads, bounds_of};
use crate::gpu;

pub const KIND: &str = "pts.file";

/// The extensions the node reads.
pub const EXTENSIONS: &[&str] = &["ply", "xyz", "csv", "txt", "pts"];

/// Most points a file gives.
pub const MAX_POINTS: usize = 1 << 22;

/// Largest file read: 4 M points of a few dozen bytes each, with room to spare.
const MAX_FILE_BYTES: u64 = 1 << 30;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            KIND,
            "A point cloud from a file: .ply from a scanner or a 3D program, or .xyz and .csv text with a column per number. Updates when the file is saved",
        )
        .param(ParamSpec::file("file", EXTENSIONS).doc(
            "The file, relative to the project directory. .ply: ASCII or binary little endian; x y z place the points, nx ny nz, red green blue (alpha) and every other number of a vertex become attributes. .xyz, .csv, .txt, .pts: a number per column, split at commas, spaces, tabs or semicolons; a first line of names says what each column is (x y z, r g b, nx ny nz, any name). Without names the columns are x y z r g b; four or seven columns are x y z intensity (r g b), as laser scanners write them, and a first line holding only the number of points is skipped",
        ))
        .output("out", DataKind::Pts)
        .describe_output(
            "out",
            "One point per vertex or line, in the file's order. Colours stored as bytes (0 to 255) are taken as sRGB and made linear, as penumbra's colours are. Exact bounds",
        ),
        || Box::new(PtsFile::default()) as Box<dyn Operator>,
    ))
}

/// A file's points, ready to upload, and what to warn about.
#[derive(Debug)]
pub(crate) struct Cloud {
    pub count: usize,
    pub columns: Vec<Column>,
    pub warnings: Vec<String>,
}

#[derive(Default)]
struct PtsFile {
    slot: AsyncSlot<std::sync::Arc<Cloud>>,
    watch: Option<FileWatch>,
    uploads: Uploads,
    sent: Option<Points>,
}

impl Operator for PtsFile {
    fn gpu_bytes(&self) -> u64 {
        self.uploads.bytes()
    }

    fn trim(&mut self) {
        self.uploads.trim();
    }

    fn job_status(&self) -> Option<JobStatus> {
        Some(self.slot.status())
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let mut fresh = self.slot.poll();
        let path = cx.resolve_path(cx.text("file")?)?;
        let moved = self.watch.as_ref().is_none_or(|w| w.path() != path);
        let changed = self.watch.as_ref().is_some_and(FileWatch::take_changed);
        if moved || changed {
            if moved {
                self.watch = Some(cx.watch_file(&path));
            }
            let p = path.clone();
            self.slot.start(cx.spawn(move |_job| read(&p).map(std::sync::Arc::new)));
        }
        if cx.exact_frames() && self.slot.busy() {
            fresh |= super::program::finish(&mut self.slot);
        }
        if let Some(e) = self.slot.error().filter(|_| !self.slot.busy()) {
            return Err(CookError::failed(e));
        }
        match (fresh, self.slot.latest().cloned(), &self.sent) {
            (true, Some(cloud), _) | (false, Some(cloud), None) => {
                if !cloud.warnings.is_empty() {
                    cx.warn(cloud.warnings.join("; "));
                }
                let bounds = cloud.columns.iter().find(|c| c.name == "position").and_then(bounds_of);
                let points = self.uploads.upload(gpu, cloud.count, &cloud.columns, bounds)?;
                self.sent = Some(points.clone());
                Ok(Cooked::Outputs(vec![Data::new(DataKind::Pts, points)]))
            }
            (_, None, _) => Err(CookError::Pending),
            (false, Some(cloud), Some(_)) => {
                if !cloud.warnings.is_empty() {
                    cx.warn(cloud.warnings.join("; "));
                }
                Ok(Cooked::Unchanged)
            }
        }
    }
}

/// Reads and parses the file at `path`.
pub(crate) fn read(path: &Path) -> Result<Cloud, String> {
    let name = path.display();
    let len = std::fs::metadata(path).map_err(|e| format!("{name}: {e}"))?.len();
    if len > MAX_FILE_BYTES {
        return Err(format!("{name}: {len} bytes, a point file may have {MAX_FILE_BYTES}"));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{name}: {e}"))?;
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    let cloud =
        if ext.as_deref() == Some("ply") || bytes.starts_with(b"ply") { ply(&bytes) } else { text(&bytes) };
    cloud.map_err(|e| format!("{name}: {e}"))
}

/// A number type of a PLY property: its size in bytes and how to read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Num {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl Num {
    fn of(name: &str) -> Option<Self> {
        Some(match name {
            "char" | "int8" => Num::I8,
            "uchar" | "uint8" => Num::U8,
            "short" | "int16" => Num::I16,
            "ushort" | "uint16" => Num::U16,
            "int" | "int32" => Num::I32,
            "uint" | "uint32" => Num::U32,
            "float" | "float32" => Num::F32,
            "double" | "float64" => Num::F64,
            _ => return None,
        })
    }

    fn size(self) -> usize {
        match self {
            Num::I8 | Num::U8 => 1,
            Num::I16 | Num::U16 => 2,
            Num::I32 | Num::U32 | Num::F32 => 4,
            Num::F64 => 8,
        }
    }

    /// The number at the start of `b`, little endian; `None` past the end.
    fn read(self, b: &[u8]) -> Option<f64> {
        let take = |n: usize| b.get(..n);
        Some(match self {
            Num::I8 => f64::from(i8::from_le_bytes(take(1)?.try_into().ok()?)),
            Num::U8 => f64::from(u8::from_le_bytes(take(1)?.try_into().ok()?)),
            Num::I16 => f64::from(i16::from_le_bytes(take(2)?.try_into().ok()?)),
            Num::U16 => f64::from(u16::from_le_bytes(take(2)?.try_into().ok()?)),
            Num::I32 => f64::from(i32::from_le_bytes(take(4)?.try_into().ok()?)),
            Num::U32 => f64::from(u32::from_le_bytes(take(4)?.try_into().ok()?)),
            Num::F32 => f64::from(f32::from_le_bytes(take(4)?.try_into().ok()?)),
            Num::F64 => f64::from_le_bytes(take(8)?.try_into().ok()?),
        })
    }
}

/// An 8-bit sRGB value (0 to 255) as linear light.
fn linear(v: f64) -> f64 {
    let c = (v / 255.0).clamp(0.0, 1.0);
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// The attribute and component a PLY property fills; `None` for one left out.
fn ply_target(name: &str) -> Option<(String, usize)> {
    let fixed = |a: &str, k: usize| Some((a.to_string(), k));
    match name {
        "x" => fixed("position", 0),
        "y" => fixed("position", 1),
        "z" => fixed("position", 2),
        "nx" => fixed("normal", 0),
        "ny" => fixed("normal", 1),
        "nz" => fixed("normal", 2),
        "red" | "r" | "diffuse_red" => fixed("color", 0),
        "green" | "g" | "diffuse_green" => fixed("color", 1),
        "blue" | "b" | "diffuse_blue" => fixed("color", 2),
        "alpha" | "a" | "diffuse_alpha" => fixed("color", 3),
        "u" | "s" | "texture_u" => fixed("uv", 0),
        "v" | "t" | "texture_v" => fixed("uv", 1),
        // Any other number: an attribute of its name, where it can be one.
        other => valid_name(other).then(|| (other.to_string(), 0)),
    }
}

/// A PLY file: its vertices' numbers.
fn ply(bytes: &[u8]) -> Result<Cloud, String> {
    // The header is text up to "end_header" and its line break.
    let end = find(bytes, b"end_header").ok_or("no end_header: not a complete PLY header")?;
    let body_at =
        bytes.get(end..).and_then(|rest| rest.iter().position(|b| *b == b'\n')).map(|k| end + k + 1);
    let body_at = body_at.ok_or("the header ends without a line break")?;
    let header = String::from_utf8_lossy(bytes.get(..end).unwrap_or(&[]));
    let mut lines = header.lines().map(str::trim);
    if lines.next() != Some("ply") {
        return Err("a PLY file starts with the line `ply`".into());
    }
    let mut binary = None;
    let mut claimed: Option<usize> = None;
    let mut props: Vec<(String, Num)> = Vec::new();
    let mut in_vertex = false;
    let mut before_vertex = false;
    for line in lines {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["format", "ascii", ..] => binary = Some(false),
            ["format", "binary_little_endian", ..] => binary = Some(true),
            ["format", other, ..] => {
                return Err(format!("format {other} is not read: ascii and binary_little_endian are"));
            }
            ["element", "vertex", n] => {
                if claimed.is_some() {
                    return Err("two vertex elements".into());
                }
                claimed = Some(n.parse::<usize>().map_err(|_| format!("`{n}` vertices is no count"))?);
                in_vertex = true;
            }
            ["element", ..] => {
                if claimed.is_none() {
                    before_vertex = true;
                }
                in_vertex = false;
            }
            ["property", "list", ..] if in_vertex => {
                return Err("a vertex with a list property is not read".into());
            }
            ["property", ty, name] if in_vertex => {
                let num = Num::of(ty).ok_or_else(|| format!("property type `{ty}` is not known"))?;
                props.push(((*name).to_string(), num));
            }
            _ => {}
        }
    }
    let binary = binary.ok_or("the header names no format")?;
    let claimed = claimed.ok_or("the file has no vertex element")?;
    if before_vertex {
        return Err("the vertex element must come first".into());
    }
    if props.is_empty() {
        return Err("the vertices have no properties".into());
    }
    let body = bytes.get(body_at..).unwrap_or(&[]);
    let mut warnings = Vec::new();
    // Where each property goes, and whether it is a colour byte to make linear.
    let targets: Vec<Option<(String, usize, bool)>> = props
        .iter()
        .map(|(name, num)| ply_target(name).map(|(a, k)| (a.clone(), k, a == "color" && *num == Num::U8)))
        .collect();
    // A colour byte: red, green and blue in sRGB, alpha straight.
    let byte = |v: f64, k: usize| if k == 3 { (v / 255.0).clamp(0.0, 1.0) } else { linear(v) };
    let targets: Vec<Option<(String, usize, bool)>> = targets.into_iter().collect();
    let mut rows: Vec<Vec<f64>> = Vec::new();
    let wanted = claimed.min(MAX_POINTS);
    if claimed > MAX_POINTS {
        warnings.push(format!("{claimed} vertices; the first {MAX_POINTS} become points"));
    }
    if binary {
        let stride: usize = props.iter().map(|(_, n)| n.size()).sum();
        let there = body.len() / stride.max(1);
        let n = wanted.min(there);
        if n < wanted {
            warnings.push(format!("the header claims {claimed} vertices, the file holds {there}"));
        }
        rows.reserve(n);
        for v in 0..n {
            let mut at = v * stride;
            let mut row = Vec::with_capacity(props.len());
            for (_, num) in &props {
                row.push(body.get(at..).and_then(|b| num.read(b)).unwrap_or(0.0));
                at += num.size();
            }
            rows.push(row);
        }
    } else {
        let text = String::from_utf8_lossy(body);
        for line in text.lines().filter(|l| !l.trim().is_empty()).take(wanted) {
            let row: Vec<f64> =
                line.split_whitespace().take(props.len()).map(|w| w.parse::<f64>().unwrap_or(0.0)).collect();
            rows.push(row);
        }
        if rows.len() < wanted {
            warnings.push(format!("the header claims {claimed} vertices, the file holds {}", rows.len()));
        }
    }
    let names: Vec<String> = targets
        .iter()
        .zip(&props)
        .map(|(t, (raw, _))| match t {
            Some((a, k, _)) if a == "position" || a == "normal" || a == "color" || a == "uv" => {
                format!("{a}.{}", ["x", "y", "z", "w"].get(*k).copied().unwrap_or("x"))
            }
            Some((a, _, _)) => a.clone(),
            None => raw.clone(),
        })
        .collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let value = |i: usize, p: usize| {
        let v = rows.get(p).and_then(|r| r.get(i)).copied()?;
        Some(match targets.get(i) {
            Some(Some((_, k, true))) => byte(v, *k),
            _ => v,
        })
    };
    let count = rows.len();
    let (cols, mut more) = columns(&refs, &value, count);
    warnings.append(&mut more);
    check(&cols)?;
    Ok(Cloud { count, columns: cols, warnings })
}

/// Where `needle` first starts in `hay`.
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// A text file: a number per column, an optional first line of names.
fn text(bytes: &[u8]) -> Result<Cloud, String> {
    let text = String::from_utf8_lossy(bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let split = |line: &str| -> Vec<String> {
        line.split([',', ' ', '\t', ';'])
            .map(|w| w.trim().trim_matches('"').trim())
            .filter(|w| !w.is_empty())
            .map(str::to_string)
            .collect()
    };
    let mut lines =
        text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with("//"));
    let mut first = lines.next().map(split).unwrap_or_default();
    // A .pts file from a laser scanner starts with the number of points on a line of its own:
    // a claim, not a point.
    if let [n] = first.as_slice()
        && n.parse::<u64>().is_ok()
    {
        first = lines.next().map(split).unwrap_or_default();
    }
    let named = first.iter().any(|w| w.parse::<f64>().is_err());
    let mut warnings = Vec::new();
    let mut rows: Vec<Vec<f64>> = Vec::new();
    if !named && !first.is_empty() {
        rows.push(first.iter().map(|w| w.parse::<f64>().unwrap_or(0.0)).collect());
    }
    for line in lines {
        if rows.len() >= MAX_POINTS {
            warnings.push(format!("more than {MAX_POINTS} lines; the first {MAX_POINTS} become points"));
            break;
        }
        rows.push(split(line).iter().map(|w| w.parse::<f64>().unwrap_or(0.0)).collect());
    }
    let count = rows.len();
    let (names, bytes_colour): (Vec<String>, bool) = if named {
        (first.clone(), false)
    } else {
        let widest = rows.iter().map(Vec::len).max().unwrap_or(0);
        // Four or seven columns, as laser scanners write them: an intensity after x y z.
        let order: &[&str] = match widest {
            4 => &["x", "y", "z", "intensity"],
            7 => &["x", "y", "z", "intensity", "r", "g", "b"],
            _ => &["x", "y", "z", "r", "g", "b"],
        };
        let mut names: Vec<String> = order.iter().take(widest).map(|s| (*s).to_string()).collect();
        if widest > order.len() {
            warnings.push(format!(
                "{widest} columns without names: the first {} are {}, the rest are left out",
                order.len(),
                order.join(" ")
            ));
            names.extend((order.len()..widest).map(|_| String::new()));
        }
        // Colours of 0 to 255 are bytes in sRGB; up to 1 they are taken as they are.
        let colour: Vec<usize> = names
            .iter()
            .enumerate()
            .filter(|(_, n)| ["r", "g", "b"].contains(&n.as_str()))
            .map(|(i, _)| i)
            .collect();
        let bytes = rows.iter().any(|r| colour.iter().any(|i| r.get(*i).is_some_and(|v| *v > 1.0)));
        (names, bytes)
    };
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let is_colour = |i: usize| refs.get(i).is_some_and(|n| ["r", "g", "b"].contains(n));
    let value = |i: usize, p: usize| {
        let v = rows.get(p).and_then(|r| r.get(i)).copied()?;
        Some(if bytes_colour && is_colour(i) { linear(v) } else { v })
    };
    let (cols, mut more) = columns(&refs, &value, count);
    warnings.append(&mut more);
    check(&cols)?;
    Ok(Cloud { count, columns: cols, warnings })
}

fn check(cols: &[Column]) -> Result<(), String> {
    if cols.len() > MAX_ATTRS {
        return Err(format!("{} attributes are more than the {MAX_ATTRS} points hold", cols.len()));
    }
    if cols.iter().any(|c| c.ty == AttrType::Vec4 && c.name == "position") {
        return Err("position has three numbers".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn floats(c: &Column) -> Vec<f32> {
        c.words.iter().map(|w| f32::from_bits(*w)).collect()
    }

    fn col<'a>(cloud: &'a Cloud, name: &str) -> &'a Column {
        cloud.columns.iter().find(|c| c.name == name).unwrap_or_else(|| panic!("no {name}"))
    }

    #[test]
    fn an_ascii_ply_gives_its_vertices() {
        let file = "ply\nformat ascii 1.0\nelement vertex 2\nproperty float x\nproperty float y\nproperty float z\n\
                    property uchar red\nproperty uchar green\nproperty uchar blue\nproperty float intensity\n\
                    element face 1\nproperty list uchar int vertex_indices\nend_header\n\
                    1 2 3 255 0 0 0.5\n4 5 6 0 255 0 0.25\n3 0 1 1\n";
        let c = ply(file.as_bytes()).unwrap();
        assert_eq!(c.count, 2);
        assert_eq!(floats(col(&c, "position")), [1.0, 2.0, 3.0, 1.0, 4.0, 5.0, 6.0, 1.0]);
        assert_eq!(floats(col(&c, "color"))[..4], [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(floats(col(&c, "intensity")), [0.5, 0.25]);
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
    }

    #[test]
    fn a_binary_ply_is_capped_by_what_it_holds() {
        let mut file = b"ply\nformat binary_little_endian 1.0\nelement vertex 1000000\nproperty float x\nproperty float y\nproperty float z\nproperty double nx\nend_header\n".to_vec();
        for v in 0..3 {
            for x in [v as f32, 0.5, -1.0] {
                file.extend(x.to_le_bytes());
            }
            file.extend(1.0f64.to_le_bytes());
        }
        // A cut vertex at the end.
        file.extend([0u8; 5]);
        let c = ply(&file).unwrap();
        assert_eq!(c.count, 3, "the header's claim is not believed");
        assert_eq!(floats(col(&c, "position"))[8..12], [2.0, 0.5, -1.0, 1.0]);
        assert_eq!(floats(col(&c, "normal"))[..3], [1.0, 0.0, 1.0]);
        assert!(c.warnings.iter().any(|w| w.contains("1000000")), "{:?}", c.warnings);
    }

    #[test]
    fn broken_files_are_errors_never_panics() {
        for bad in [
            &b""[..],
            b"ply",
            b"ply\nformat ascii 1.0\n",
            b"ply\nformat binary_big_endian 1.0\nelement vertex 1\nproperty float x\nend_header\n",
            b"ply\nformat ascii 1.0\nelement vertex lots\nend_header\n",
            b"ply\nformat ascii 1.0\nelement vertex 1\nproperty quad x\nend_header\n",
            b"ply\nformat ascii 1.0\nelement vertex 1\nproperty list uchar int x\nend_header\n",
            b"ply\nformat ascii 1.0\nelement face 1\nelement vertex 1\nproperty float x\nend_header\n",
        ] {
            assert!(ply(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
        // Too few numbers on a line: the missing ones are 0, as is text that is no number.
        let c = ply(b"ply\nformat ascii 1.0\nelement vertex 2\nproperty float x\nproperty float y\nend_header\n1\nq 2\n")
            .unwrap();
        assert_eq!(floats(col(&c, "position")), [1.0, 0.0, 0.0, 1.0, 0.0, 2.0, 0.0, 1.0]);
        assert!(text(b"").is_ok_and(|c| c.count == 0));
        assert!(text(b"\xff\xfe garbage \x00").is_ok());
    }

    #[test]
    fn text_files_take_names_or_x_y_z_r_g_b() {
        let c = text(b"# a comment\n1 2 3 255 255 0\n4,5,6,0,0,0\n").unwrap();
        assert_eq!(c.count, 2);
        assert_eq!(floats(col(&c, "position"))[4..7], [4.0, 5.0, 6.0]);
        assert_eq!(floats(col(&c, "color"))[..4], [1.0, 1.0, 0.0, 1.0]);
        let c = text(b"\"x\";\"y\";\"z\";\"weight\"\n1;2;3;0.5\n").unwrap();
        assert_eq!((c.count, floats(col(&c, "weight"))), (1, vec![0.5]));
        // A scanner's .pts: the count first, then x y z intensity r g b.
        let c = text(b"2\n1 2 3 -500 255 0 0\n4 5 6 20 0 0 255\n").unwrap();
        assert_eq!(c.count, 2);
        assert_eq!(floats(col(&c, "position"))[..3], [1.0, 2.0, 3.0]);
        assert_eq!(floats(col(&c, "intensity")), [-500.0, 20.0]);
        assert_eq!(floats(col(&c, "color"))[4..8], [0.0, 0.0, 1.0, 1.0]);
    }
}
