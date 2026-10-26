//! Writers for OBJ, binary STL and `.glb`: test files are written from code (no downloaded
//! models), and a mesh can be handed to other tools. Parts survive as OBJ `o` and glTF nodes;
//! STL has none.
//!
//! The mesh is z up (penumbra's world); glTF (y up by its spec) and OBJ (y up by habit) are
//! written y up, STL z up, so each reads back right with `geo.file`'s `up: auto`.

use std::fmt::Write as _;

use crate::Mesh;

/// OBJ text, one `o` per range's part (in range order), positions, texture coordinates
/// (flipped back to OBJ's bottom left origin) and normals. With `mtllib`, a `usemtl` per
/// material run (the library itself is not written).
pub fn to_obj(mesh: &Mesh, mtllib: Option<&str>) -> String {
    let mut s = String::new();
    if let Some(lib) = mtllib {
        let _ = writeln!(s, "mtllib {lib}");
    }
    for v in &mesh.vertices {
        let [x, y, z] = y_up(v.position);
        let _ = writeln!(s, "v {x} {y} {z}");
    }
    for v in &mesh.vertices {
        let [u, t] = v.uv;
        let _ = writeln!(s, "vt {u} {}", 1.0 - t);
    }
    for v in &mesh.vertices {
        let [x, y, z] = y_up(v.normal);
        let _ = writeln!(s, "vn {x} {y} {z}");
    }
    let mut part = u32::MAX;
    let mut material = u32::MAX;
    for r in &mesh.ranges {
        if r.part != part {
            part = r.part;
            let _ = writeln!(s, "o {}", mesh.parts.get(r.part as usize).map_or("part", String::as_str));
        }
        if mtllib.is_some() && r.material != material {
            material = r.material;
            let _ = writeln!(
                s,
                "usemtl {}",
                mesh.materials.get(r.material as usize).map_or("default", |m| &m.name)
            );
        }
        let start = r.first as usize;
        let end = start + r.count as usize;
        for t in mesh.indices.get(start..end).unwrap_or(&[]).as_chunks::<3>().0 {
            let _ = write!(s, "f");
            for i in t {
                let k = i + 1;
                let _ = write!(s, " {k}/{k}/{k}");
            }
            let _ = writeln!(s);
        }
    }
    s
}

/// Binary STL.
pub fn to_stl(mesh: &Mesh) -> Vec<u8> {
    let mut out = vec![0u8; 80];
    let n = mesh.triangle_count() as u32;
    out.extend(n.to_le_bytes());
    for t in mesh.indices.as_chunks::<3>().0 {
        let p: Vec<[f32; 3]> =
            t.iter().map(|i| mesh.vertices.get(*i as usize).map_or([0.0; 3], |v| v.position)).collect();
        let normal = match p.as_slice() {
            [a, b, c] => crate::face_normal([*a, *b, *c]),
            _ => [0.0; 3],
        };
        for c in normal.iter().chain(p.iter().flatten()) {
            out.extend(c.to_le_bytes());
        }
        out.extend([0u8, 0]);
    }
    out
}

/// A `.glb`: one node and mesh per part (named after the part), one primitive per range,
/// positions, normals, texture coordinates and 32-bit indices in one buffer; materials with
/// their base colour (images are not written).
pub fn to_glb(mesh: &Mesh) -> Vec<u8> {
    let mut bin: Vec<u8> = Vec::new();
    let mut views = Vec::new();
    let mut accessors = Vec::new();
    let mut push_view = |bin: &mut Vec<u8>, bytes: &[u8]| -> usize {
        while !bin.len().is_multiple_of(4) {
            bin.push(0);
        }
        let offset = bin.len();
        bin.extend_from_slice(bytes);
        views.push(serde_json::json!({"buffer": 0, "byteOffset": offset, "byteLength": bytes.len()}));
        views.len() - 1
    };
    let mut meshes = Vec::new();
    let mut nodes = Vec::new();
    for (part, name) in mesh.parts.iter().enumerate() {
        let mut primitives = Vec::new();
        for r in mesh.ranges.iter().filter(|r| r.part as usize == part) {
            let start = r.first as usize;
            let tri = mesh.indices.get(start..start + r.count as usize).unwrap_or(&[]);
            // Local vertices of this range, renumbered.
            let mut map = std::collections::HashMap::new();
            let mut local = Vec::new();
            let mut idx = Vec::new();
            for i in tri {
                let k = *map.entry(*i).or_insert_with(|| {
                    local.push(mesh.vertices.get(*i as usize).copied().unwrap_or_default());
                    local.len() as u32 - 1
                });
                idx.push(k);
            }
            let bytes = |f: &dyn Fn(&crate::Vertex) -> Vec<f32>| -> Vec<u8> {
                local.iter().flat_map(f).flat_map(f32::to_le_bytes).collect()
            };
            let pos = bytes(&|v| y_up(v.position).to_vec());
            let nor = bytes(&|v| y_up(v.normal).to_vec());
            let uv = bytes(&|v| v.uv.to_vec());
            let ind: Vec<u8> = idx.iter().flat_map(|i| i.to_le_bytes()).collect();
            let b = crate::Bounds::of(local.iter().map(|v| y_up(v.position)));
            let (min, max) = b.map_or(([0.0; 3], [0.0; 3]), |b| (b.min, b.max));
            let mut acc = |view: usize, count: usize, ty: &str, ct: u32, extra: serde_json::Value| {
                let mut a =
                    serde_json::json!({"bufferView": view, "count": count, "type": ty, "componentType": ct});
                if let (Some(o), Some(e)) = (a.as_object_mut(), extra.as_object()) {
                    o.extend(e.clone());
                }
                accessors.push(a);
                accessors.len() - 1
            };
            let n = local.len();
            let v0 = push_view(&mut bin, &pos);
            let a_pos = acc(v0, n, "VEC3", 5126, serde_json::json!({"min": min, "max": max}));
            let v1 = push_view(&mut bin, &nor);
            let a_nor = acc(v1, n, "VEC3", 5126, serde_json::json!({}));
            let v2 = push_view(&mut bin, &uv);
            let a_uv = acc(v2, n, "VEC2", 5126, serde_json::json!({}));
            let v3 = push_view(&mut bin, &ind);
            let a_ind = acc(v3, idx.len(), "SCALAR", 5125, serde_json::json!({}));
            primitives.push(serde_json::json!({
                "attributes": {"POSITION": a_pos, "NORMAL": a_nor, "TEXCOORD_0": a_uv},
                "indices": a_ind,
                "material": r.material,
            }));
        }
        meshes.push(serde_json::json!({"name": name, "primitives": primitives}));
        nodes.push(serde_json::json!({"name": name, "mesh": meshes.len() - 1}));
    }
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    let materials: Vec<serde_json::Value> = mesh
        .materials
        .iter()
        .map(|m| serde_json::json!({"name": m.name, "pbrMetallicRoughness": {"baseColorFactor": m.color}}))
        .collect();
    let doc = serde_json::json!({
        "asset": {"version": "2.0", "generator": "penumbra-mesh"},
        "scene": 0,
        "scenes": [{"nodes": (0..nodes.len()).collect::<Vec<_>>()}],
        "nodes": nodes,
        "meshes": meshes,
        "materials": materials,
        "accessors": accessors,
        "bufferViews": views,
        "buffers": [{"byteLength": bin.len()}],
    });
    let mut json = doc.to_string().into_bytes();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let total = 12 + 8 + json.len() + 8 + bin.len();
    let mut out = Vec::with_capacity(total);
    out.extend(b"glTF");
    out.extend(2u32.to_le_bytes());
    out.extend((total as u32).to_le_bytes());
    out.extend((json.len() as u32).to_le_bytes());
    out.extend(0x4E4F_534Au32.to_le_bytes());
    out.extend(json);
    out.extend((bin.len() as u32).to_le_bytes());
    out.extend(0x004E_4942u32.to_le_bytes());
    out.extend(bin);
    out
}

/// z up to y up, the inverse of [`crate::Mesh::to_z_up`].
fn y_up([x, y, z]: [f32; 3]) -> [f32; 3] {
    [x, z, -y]
}

/// Several meshes as one, each mesh's parts and materials kept (renumbered), for test scenes.
pub fn combine(meshes: &[Mesh]) -> Mesh {
    let mut out = Mesh::default();
    for m in meshes {
        out.append(m);
    }
    out
}
