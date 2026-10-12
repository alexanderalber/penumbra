//! glTF 2.0, `.gltf` (JSON with external or `data:` buffers) and `.glb`. Read: the default
//! scene's node tree with each node's transform baked into the vertices, triangle primitives
//! (lists, strips, fans) with positions, normals and the first texture coordinates, the base
//! colour and base colour texture of materials. Every node with a mesh becomes a part, named
//! after the node (else its mesh). Not read: skins, morph targets, animations, cameras,
//! lights, sparse accessors, the other PBR textures.
//!
//! Every accessor is checked against its buffer view and buffer with checked arithmetic, and
//! its count against the [`Limits`], before anything is allocated for it. The JSON itself is
//! counted first (values, not bytes), so a small file of many tiny values cannot balloon in
//! memory.

use std::collections::HashMap;

use serde_json::Value;

use crate::{Builder, Error, Image, Limits, Loaded, Material, Vertex, face_normal, normalize};

/// Most JSON values (numbers, strings, objects, ...) in the JSON part. A parsed value costs
/// about 30 to 80 bytes, so this caps the parsed document at a few hundred MiB.
const MAX_JSON_VALUES: usize = 4 << 20;

type Mat = [[f64; 4]; 4];
const IDENTITY: Mat =
    [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];

pub fn parse(
    bytes: &[u8],
    sidecar: &dyn Fn(&str) -> Result<Vec<u8>, Error>,
    limits: &Limits,
) -> Result<Loaded, Error> {
    let (json, bin) = if bytes.starts_with(b"glTF") { split_glb(bytes)? } else { (bytes, None) };
    if json_values(json) > MAX_JSON_VALUES {
        return Err(Error::new(format!("the JSON has more than {MAX_JSON_VALUES} values")));
    }
    let doc: Value = serde_json::from_slice(json).map_err(|e| Error::new(format!("JSON: {e}")))?;
    if !doc.is_object() {
        return Err(Error::new("the JSON is not a glTF object"));
    }
    if let Some(v) = doc.pointer("/asset/version").and_then(Value::as_str)
        && !v.starts_with('2')
    {
        return Err(Error::new(format!("glTF version {v}; only 2.x is read")));
    }
    let mut r = Reader {
        doc: &doc,
        bin,
        sidecar,
        limits,
        buffers: HashMap::new(),
        total: bytes.len() as u64,
        materials: HashMap::new(),
        images: HashMap::new(),
        b: Builder::new(limits),
        warnings: Vec::new(),
    };
    r.walk()?;
    let Reader { b, mut warnings, .. } = r;
    warnings.dedup();
    Ok(Loaded { mesh: b.finish(), warnings })
}

/// The JSON chunk and the optional BIN chunk of a `.glb`.
fn split_glb(bytes: &[u8]) -> Result<(&[u8], Option<&[u8]>), Error> {
    let u32_at = |at: usize| -> Option<usize> {
        let b: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
        Some(u32::from_le_bytes(b) as usize)
    };
    let bad = || Error::new("not a valid .glb (header or chunks)");
    if u32_at(4) != Some(2) {
        return Err(Error::new("only .glb version 2 is read"));
    }
    let length = u32_at(8).ok_or_else(bad)?.min(bytes.len());
    let json_len = u32_at(12).ok_or_else(bad)?;
    if u32_at(16) != Some(0x4E4F_534A) {
        return Err(bad());
    }
    let json_end = 20usize.checked_add(json_len).ok_or_else(bad)?;
    let json = bytes.get(20..json_end).ok_or_else(bad)?;
    let mut bin = None;
    let bin_start = json_end.checked_add(8).ok_or_else(bad)?;
    if bin_start <= length {
        let bin_len = u32_at(json_end).ok_or_else(bad)?;
        if u32_at(json_end + 4) == Some(0x004E_4942) {
            let end = bin_start.checked_add(bin_len).ok_or_else(bad)?;
            bin = Some(bytes.get(bin_start..end).ok_or_else(bad)?);
        }
    }
    Ok((json, bin))
}

/// An upper bound of the values in a JSON text: one more than its commas, colons, opening
/// brackets and braces outside strings.
fn json_values(json: &[u8]) -> usize {
    let mut n = 1usize;
    let mut in_string = false;
    let mut escaped = false;
    for &c in json {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_string = false;
            }
        } else {
            match c {
                b'"' => in_string = true,
                b',' | b':' | b'[' | b'{' => n += 1,
                _ => {}
            }
        }
    }
    n
}

/// Where an accessor's elements lie in a loaded buffer.
struct Layout {
    buffer: usize,
    start: usize,
    stride: usize,
    count: usize,
    csize: usize,
    component: usize,
    normalized: bool,
}

struct Reader<'a> {
    doc: &'a Value,
    bin: Option<&'a [u8]>,
    sidecar: &'a dyn Fn(&str) -> Result<Vec<u8>, Error>,
    limits: &'a Limits,
    buffers: HashMap<usize, Vec<u8>>,
    /// Bytes read so far: the file, buffers, images.
    total: u64,
    /// glTF material index to ours.
    materials: HashMap<usize, u32>,
    /// glTF image index to ours (`None`: could not be read, warned once).
    images: HashMap<usize, Option<u32>>,
    b: Builder<'a>,
    warnings: Vec<String>,
}

fn array<'v>(v: &'v Value, key: &str) -> &'v [Value] {
    v.get(key).and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

fn index(v: &Value, key: &str) -> Option<usize> {
    v.get(key).and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok())
}

fn numbers<const N: usize>(v: Option<&Value>) -> Option<[f64; N]> {
    let a = v?.as_array()?;
    if a.len() != N {
        return None;
    }
    let mut out = [0.0; N];
    for (o, x) in out.iter_mut().zip(a) {
        *o = x.as_f64().filter(|f| f.is_finite())?;
    }
    Some(out)
}

impl<'a> Reader<'a> {
    fn item(&self, kind: &str, i: usize) -> Result<&'a Value, Error> {
        array(self.doc, kind).get(i).ok_or_else(|| Error::new(format!("{kind}[{i}] does not exist")))
    }

    fn walk(&mut self) -> Result<(), Error> {
        let nodes = array(self.doc, "nodes");
        let roots: Vec<usize> = match self.doc.get("scenes").and_then(Value::as_array) {
            Some(scenes) if !scenes.is_empty() => {
                let s = index(self.doc, "scene").unwrap_or(0);
                let scene = scenes.get(s).ok_or_else(|| Error::new(format!("scene {s} does not exist")))?;
                array(scene, "nodes").iter().filter_map(|v| v.as_u64().map(|i| i as usize)).collect()
            }
            _ => {
                // No scene: every node that is nobody's child.
                let mut child = vec![false; nodes.len()];
                for n in nodes {
                    for c in array(n, "children").iter().filter_map(Value::as_u64) {
                        if let Some(slot) = child.get_mut(c as usize) {
                            *slot = true;
                        }
                    }
                }
                (0..nodes.len()).filter(|i| !child.get(*i).copied().unwrap_or(true)).collect()
            }
        };
        let mut stack: Vec<(usize, Mat, usize)> = roots.into_iter().rev().map(|r| (r, IDENTITY, 0)).collect();
        let mut visits = 0usize;
        while let Some((i, parent, depth)) = stack.pop() {
            visits += 1;
            if visits > self.limits.max_node_visits {
                return Err(Error::new(format!("more than {} node visits", self.limits.max_node_visits)));
            }
            if depth > self.limits.max_depth {
                return Err(Error::new(format!("nodes nested deeper than {}", self.limits.max_depth)));
            }
            let node = self.item("nodes", i)?;
            let world =
                mul(&parent, &local(node).ok_or_else(|| Error::new(format!("nodes[{i}]: bad transform")))?);
            if let Some(m) = index(node, "mesh") {
                let mesh = self.item("meshes", m)?;
                let name = node
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .or_else(|| mesh.get("name").and_then(Value::as_str).filter(|s| !s.is_empty()))
                    .map_or_else(|| format!("node{i}"), str::to_string);
                let part = self.b.new_part(&name)?;
                for (k, prim) in array(mesh, "primitives").iter().enumerate() {
                    self.primitive(prim, &world, part)
                        .map_err(|e| e.context(format!("meshes[{m}].primitives[{k}]")))?;
                }
            }
            for c in array(node, "children").iter().rev() {
                let c = c.as_u64().ok_or_else(|| Error::new(format!("nodes[{i}]: bad child")))? as usize;
                stack.push((c, world, depth + 1));
            }
        }
        Ok(())
    }

    fn primitive(&mut self, prim: &Value, world: &Mat, part: u32) -> Result<(), Error> {
        let mode = prim.get("mode").and_then(Value::as_u64).unwrap_or(4);
        if !matches!(mode, 4..=6) {
            self.warnings.push("points and lines are skipped".into());
            return Ok(());
        }
        let attributes = prim.get("attributes").ok_or_else(|| Error::new("no attributes"))?;
        let pos_acc = index(attributes, "POSITION").ok_or_else(|| Error::new("no POSITION"))?;
        let positions = self.read(pos_acc, 3, self.limits.max_vertices)?;
        let count = positions.len() / 3;
        let normals = match index(attributes, "NORMAL") {
            Some(a) => Some(self.read(a, 3, self.limits.max_vertices)?),
            None => None,
        };
        let uvs = match index(attributes, "TEXCOORD_0") {
            Some(a) => Some(self.read(a, 2, self.limits.max_vertices)?),
            None => None,
        };
        if normals.as_ref().is_some_and(|n| n.len() / 3 != count)
            || uvs.as_ref().is_some_and(|u| u.len() / 2 != count)
        {
            return Err(Error::new("attributes of different counts"));
        }
        let list: Vec<u32> = match index(prim, "indices") {
            Some(a) => self.read_indices(a)?,
            None => (0..count as u32).collect(),
        };
        if list.iter().any(|i| *i as usize >= count) {
            return Err(Error::new("an index points past the vertices"));
        }
        let triangles = triangles(&list, mode);
        let material = match index(prim, "material") {
            Some(m) => self.material(m)?,
            None => 0,
        };
        let normal_matrix = normal_matrix(world);
        let flip = det3(world) < 0.0;
        let pos = |k: usize| -> [f32; 3] {
            let p = [0, 1, 2].map(|c| f64::from(positions.get(k * 3 + c).copied().unwrap_or(0.0)));
            transform(world, p).map(|x| x as f32)
        };
        let uv = |k: usize| -> [f32; 2] {
            uvs.as_ref().map_or([0.0; 2], |u| [0, 1].map(|c| u.get(k * 2 + c).copied().unwrap_or(0.0)))
        };
        self.b.reserve_triangles(triangles.len())?;
        match &normals {
            Some(n) => {
                self.b.reserve_vertices(count)?;
                let base = self.b.vertex_count() as u32;
                for k in 0..count {
                    let raw = [0, 1, 2].map(|c| f64::from(n.get(k * 3 + c).copied().unwrap_or(0.0)));
                    // The cofactor matrix is the inverse transpose times the determinant.
                    let sign = if flip { -1.0 } else { 1.0 };
                    let normal = normalize(rotate(&normal_matrix, raw).map(|x| (x * sign) as f32));
                    self.b.vertex(Vertex { position: pos(k), normal, uv: uv(k) })?;
                }
                for t in triangles {
                    let t = if flip { [t[0], t[2], t[1]] } else { t };
                    self.b.triangle(t.map(|i| base + i), part, material)?;
                }
            }
            None => {
                self.b.reserve_vertices(triangles.len().saturating_mul(3))?;
                for t in triangles {
                    let t = if flip { [t[0], t[2], t[1]] } else { t };
                    let p = t.map(|i| pos(i as usize));
                    if face_normal(p) == [0.0; 3] {
                        continue;
                    }
                    self.b.flat_triangle(p, t.map(|i| uv(i as usize)), part, material)?;
                }
            }
        }
        Ok(())
    }

    /// The bytes of buffer `i`, read once.
    fn buffer(&mut self, i: usize) -> Result<&[u8], Error> {
        if !self.buffers.contains_key(&i) {
            let desc = self.item("buffers", i)?;
            let data = match desc.get("uri").and_then(Value::as_str) {
                Some(uri) => self.fetch(uri)?,
                None if i == 0 => self
                    .bin
                    .ok_or_else(|| Error::new("buffer 0 has no uri and there is no BIN chunk"))?
                    .to_vec(),
                None => return Err(Error::new(format!("buffers[{i}] has no uri"))),
            };
            let declared = index(desc, "byteLength").unwrap_or(0);
            if data.len() < declared {
                return Err(Error::new(format!("buffers[{i}] is shorter than its byteLength")));
            }
            self.buffers.insert(i, data);
        }
        self.buffers.get(&i).map(Vec::as_slice).ok_or_else(|| Error::new("buffer vanished"))
    }

    /// A `data:` URI decoded, or a file next to the glTF read.
    fn fetch(&mut self, uri: &str) -> Result<Vec<u8>, Error> {
        let data = if let Some(rest) = uri.strip_prefix("data:") {
            let (_, payload) =
                rest.split_once(";base64,").ok_or_else(|| Error::new("only base64 data URIs are read"))?;
            if payload.len() as u64 / 4 * 3 > self.limits.max_file_bytes {
                return Err(Error::new("an embedded buffer is too large"));
            }
            base64(payload)?
        } else {
            (self.sidecar)(uri)?
        };
        self.total = self.total.saturating_add(data.len() as u64);
        if self.total > self.limits.max_total_bytes {
            return Err(Error::new(format!(
                "buffers and images exceed {} MiB together",
                self.limits.max_total_bytes >> 20
            )));
        }
        Ok(data)
    }

    /// The components of accessor `i` as floats (normalised integers mapped to 0..1 or -1..1),
    /// `n` per element, at most `max` elements.
    fn read(&mut self, i: usize, n: usize, max: usize) -> Result<Vec<f32>, Error> {
        let l = self.layout(i, n, max)?;
        let data = self.buffers.get(&l.buffer).map_or(&[][..], Vec::as_slice);
        let mut out = Vec::with_capacity(l.count.saturating_mul(n));
        for e in 0..l.count {
            for c in 0..n {
                let at = l.start + e * l.stride + c * l.csize;
                let b = data
                    .get(at..at + l.csize)
                    .ok_or_else(|| Error::new(format!("accessors[{i}]: out of bounds")))?;
                let v = match (l.component, b) {
                    (5126, &[a, b, c, d]) => f32::from_le_bytes([a, b, c, d]),
                    (5121, &[a]) => {
                        if l.normalized {
                            f32::from(a) / 255.0
                        } else {
                            f32::from(a)
                        }
                    }
                    (5120, &[a]) => {
                        let x = f32::from(a as i8);
                        if l.normalized { (x / 127.0).max(-1.0) } else { x }
                    }
                    (5123, &[a, b]) => {
                        let x = f32::from(u16::from_le_bytes([a, b]));
                        if l.normalized { x / 65535.0 } else { x }
                    }
                    (5122, &[a, b]) => {
                        let x = f32::from(i16::from_le_bytes([a, b]));
                        if l.normalized { (x / 32767.0).max(-1.0) } else { x }
                    }
                    (5125, &[a, b, c, d]) => u32::from_le_bytes([a, b, c, d]) as f32,
                    _ => return Err(Error::new(format!("accessors[{i}]: unsupported componentType"))),
                };
                if !v.is_finite() {
                    return Err(Error::new(format!("accessors[{i}]: a value is not finite")));
                }
                out.push(v);
            }
        }
        Ok(out)
    }

    fn read_indices(&mut self, i: usize) -> Result<Vec<u32>, Error> {
        let max = self.limits.max_triangles.saturating_mul(3);
        let l = self.layout(i, 1, max)?;
        let data = self.buffers.get(&l.buffer).map_or(&[][..], Vec::as_slice);
        let mut out = Vec::with_capacity(l.count);
        for e in 0..l.count {
            let at = l.start + e * l.stride;
            let b = data
                .get(at..at + l.csize)
                .ok_or_else(|| Error::new(format!("accessors[{i}]: out of bounds")))?;
            out.push(match (l.component, b) {
                (5121, &[a]) => u32::from(a),
                (5123, &[a, b]) => u32::from(u16::from_le_bytes([a, b])),
                (5125, &[a, b, c, d]) => u32::from_le_bytes([a, b, c, d]),
                _ => return Err(Error::new(format!("accessors[{i}]: indices must be unsigned integers"))),
            });
        }
        Ok(out)
    }

    /// Where accessor `i`'s elements lie, after checking type, count, bounds and stride (and
    /// loading its buffer).
    fn layout(&mut self, i: usize, n: usize, max: usize) -> Result<Layout, Error> {
        let acc = self.item("accessors", i)?;
        let ctx = |m: &str| Error::new(format!("accessors[{i}]: {m}"));
        if acc.get("sparse").is_some() {
            return Err(ctx("sparse accessors are not read"));
        }
        let ncomp = match acc.get("type").and_then(Value::as_str) {
            Some("SCALAR") => 1,
            Some("VEC2") => 2,
            Some("VEC3") => 3,
            Some("VEC4") => 4,
            _ => return Err(ctx("bad type")),
        };
        if ncomp != n {
            return Err(ctx(&format!("{ncomp} components where {n} are expected")));
        }
        let component = index(acc, "componentType").unwrap_or(0);
        let csize = match component {
            5120 | 5121 => 1,
            5122 | 5123 => 2,
            5125 | 5126 => 4,
            _ => return Err(ctx("bad componentType")),
        };
        let normalized = acc.get("normalized").and_then(Value::as_bool).unwrap_or(false);
        let count = index(acc, "count").ok_or_else(|| ctx("no count"))?;
        if count > max {
            return Err(ctx(&format!("{count} elements, more than {max}")));
        }
        let offset = index(acc, "byteOffset").unwrap_or(0);
        let view = index(acc, "bufferView")
            .ok_or_else(|| ctx("no bufferView (zero-filled accessors are not read)"))?;
        let v = self.item("bufferViews", view)?;
        let buffer = index(v, "buffer").ok_or_else(|| ctx("its buffer view has no buffer"))?;
        let view_offset = index(v, "byteOffset").unwrap_or(0);
        let view_length = index(v, "byteLength").ok_or_else(|| ctx("its buffer view has no byteLength"))?;
        let elem = ncomp * csize;
        let stride = index(v, "byteStride").unwrap_or(elem);
        if stride < elem || stride > 252 {
            return Err(ctx("bad byteStride"));
        }
        let data_len = self.buffer(buffer)?.len();
        let view_end = view_offset.checked_add(view_length).ok_or_else(|| ctx("offsets overflow"))?;
        if view_end > data_len {
            return Err(ctx("its buffer view lies outside its buffer"));
        }
        if count > 0 {
            let last = (count - 1)
                .checked_mul(stride)
                .and_then(|x| x.checked_add(offset))
                .and_then(|x| x.checked_add(elem))
                .ok_or_else(|| ctx("offsets overflow"))?;
            if last > view_length {
                return Err(ctx("lies outside its buffer view"));
            }
        }
        Ok(Layout { buffer, start: view_offset + offset, stride, count, csize, component, normalized })
    }

    /// A copy of buffer view `i`'s bytes (an embedded image).
    fn view_bytes(&mut self, i: usize) -> Result<Vec<u8>, Error> {
        let v = self.item("bufferViews", i)?;
        let buffer =
            index(v, "buffer").ok_or_else(|| Error::new(format!("bufferViews[{i}] has no buffer")))?;
        let offset = index(v, "byteOffset").unwrap_or(0);
        let length = index(v, "byteLength")
            .ok_or_else(|| Error::new(format!("bufferViews[{i}] has no byteLength")))?;
        let data = self.buffer(buffer)?;
        let end = offset.checked_add(length).ok_or_else(|| Error::new("buffer view overflows"))?;
        Ok(data
            .get(offset..end)
            .ok_or_else(|| Error::new(format!("bufferViews[{i}] lies outside its buffer")))?
            .to_vec())
    }

    /// Our index of glTF material `m`, read once.
    fn material(&mut self, m: usize) -> Result<u32, Error> {
        if let Some(id) = self.materials.get(&m) {
            return Ok(*id);
        }
        let desc = self.item("materials", m)?;
        let name =
            desc.get("name").and_then(Value::as_str).map_or_else(|| format!("material{m}"), str::to_string);
        let pbr = desc.get("pbrMetallicRoughness");
        let color =
            numbers::<4>(pbr.and_then(|p| p.get("baseColorFactor"))).unwrap_or([1.0; 4]).map(|c| c as f32);
        let texture = match pbr.and_then(|p| p.get("baseColorTexture")) {
            Some(t) => {
                if t.get("texCoord").and_then(Value::as_u64).unwrap_or(0) != 0 {
                    self.warnings
                        .push("textures on other coordinates than TEXCOORD_0 are read on TEXCOORD_0".into());
                }
                match index(t, "index").map(|ti| self.item("textures", ti)).transpose()? {
                    Some(tex) => match index(tex, "source") {
                        Some(img) => self.image(img)?,
                        None => {
                            self.warnings
                                .push("a texture without a source (an extension's image format?)".into());
                            None
                        }
                    },
                    None => None,
                }
            }
            None => None,
        };
        let id = self.b.material(Material { name, color, texture })?;
        self.materials.insert(m, id);
        Ok(id)
    }

    /// Our index of glTF image `i`, read once; `None` (with a warning) when it cannot be read.
    fn image(&mut self, i: usize) -> Result<Option<u32>, Error> {
        if let Some(id) = self.images.get(&i) {
            return Ok(*id);
        }
        let desc = self.item("images", i)?;
        let name =
            desc.get("name").and_then(Value::as_str).map_or_else(|| format!("image{i}"), str::to_string);
        let bytes = if let Some(uri) = desc.get("uri").and_then(Value::as_str) {
            self.fetch(uri)
        } else if let Some(v) = index(desc, "bufferView") {
            self.view_bytes(v)
        } else {
            Err(Error::new("no uri and no bufferView"))
        };
        let id = match bytes {
            Ok(bytes) => Some(self.b.image(Image { name, bytes })?),
            Err(e) => {
                self.warnings.push(format!("images[{i}]: {e}"));
                None
            }
        };
        self.images.insert(i, id);
        Ok(id)
    }
}

/// Triangles of a list (4), strip (5) or fan (6).
fn triangles(list: &[u32], mode: u64) -> Vec<[u32; 3]> {
    match mode {
        5 => list
            .windows(3)
            .enumerate()
            .filter_map(|(k, w)| match *w {
                [a, b, c] => Some(if k % 2 == 0 { [a, b, c] } else { [b, a, c] }),
                _ => None,
            })
            .collect(),
        6 => {
            let Some(&first) = list.first() else { return Vec::new() };
            list.windows(2)
                .skip(1)
                .filter_map(|w| match *w {
                    [b, c] => Some([first, b, c]),
                    _ => None,
                })
                .collect()
        }
        _ => list.as_chunks::<3>().0.to_vec(),
    }
}

/// A node's local matrix: `matrix` (column-major in the file) or translation, rotation
/// (quaternion x, y, z, w) and scale. Row-major here.
fn local(node: &Value) -> Option<Mat> {
    if let Some(m) = node.get("matrix") {
        let c = numbers::<16>(Some(m))?;
        let mut out = IDENTITY;
        for (r, row) in out.iter_mut().enumerate() {
            for (k, x) in row.iter_mut().enumerate() {
                *x = *c.get(k * 4 + r)?;
            }
        }
        return Some(out);
    }
    let t = match node.get("translation") {
        Some(v) => numbers::<3>(Some(v))?,
        None => [0.0; 3],
    };
    let q = match node.get("rotation") {
        Some(v) => numbers::<4>(Some(v))?,
        None => [0.0, 0.0, 0.0, 1.0],
    };
    let s = match node.get("scale") {
        Some(v) => numbers::<3>(Some(v))?,
        None => [1.0; 3],
    };
    let len = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    let [x, y, z, w] = if len > 1e-12 { q.map(|c| c / len) } else { [0.0, 0.0, 0.0, 1.0] };
    let r = [
        [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y - z * w), 2.0 * (x * z + y * w)],
        [2.0 * (x * y + z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z - x * w)],
        [2.0 * (x * z - y * w), 2.0 * (y * z + x * w), 1.0 - 2.0 * (x * x + y * y)],
    ];
    Some([
        [r[0][0] * s[0], r[0][1] * s[1], r[0][2] * s[2], t[0]],
        [r[1][0] * s[0], r[1][1] * s[1], r[1][2] * s[2], t[1]],
        [r[2][0] * s[0], r[2][1] * s[1], r[2][2] * s[2], t[2]],
        [0.0, 0.0, 0.0, 1.0],
    ])
}

fn dot4(a: &[f64; 4], b: &[f64; 4]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn transpose(m: &Mat) -> Mat {
    let [[a, b, c, d], [e, f, g, h], [i, j, k, l], [m0, n, o, p]] = *m;
    [[a, e, i, m0], [b, f, j, n], [c, g, k, o], [d, h, l, p]]
}

pub(crate) fn mul(a: &Mat, b: &Mat) -> Mat {
    let bt = transpose(b);
    a.map(|row| bt.map(|col| dot4(&row, &col)))
}

pub(crate) fn transform(m: &Mat, p: [f64; 3]) -> [f64; 3] {
    let v = [p[0], p[1], p[2], 1.0];
    let [r0, r1, r2, _] = m;
    [dot4(r0, &v), dot4(r1, &v), dot4(r2, &v)]
}

fn rotate(m: &[[f64; 3]; 3], n: [f64; 3]) -> [f64; 3] {
    m.map(|row| row.iter().zip(&n).map(|(a, b)| a * b).sum())
}

fn det3(m: &Mat) -> f64 {
    let [[a, b, c, _], [d, e, f, _], [g, h, i, _], _] = *m;
    a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g)
}

/// The inverse transpose of the upper 3x3 (the cofactor matrix; its scale does not matter,
/// normals are normalised afterwards).
fn normal_matrix(m: &Mat) -> [[f64; 3]; 3] {
    let [[a, b, c, _], [d, e, f, _], [g, h, i, _], _] = *m;
    [
        [e * i - f * h, f * g - d * i, d * h - e * g],
        [c * h - b * i, a * i - c * g, b * g - a * h],
        [b * f - c * e, c * d - a * f, a * e - b * d],
    ]
}

/// Standard base64 (padding optional, no line breaks).
fn base64(s: &str) -> Result<Vec<u8>, Error> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3 + 3);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => return Err(Error::new("bad base64 in a data URI")),
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

/// Exposed for the writer's round trip tests.
#[cfg(test)]
pub(crate) fn base64_for_tests(s: &str) -> Result<Vec<u8>, Error> {
    base64(s)
}
