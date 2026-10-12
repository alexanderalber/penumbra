//! Wavefront OBJ with MTL materials. Parts come from `o` and `g` (the latest one names the part
//! of the faces that follow); faces are polygons, split into fans. Faces without normals get
//! their own flat normal. `vt` is flipped (OBJ's origin is the bottom left).

use std::collections::HashMap;

use crate::{Builder, Error, Image, Limits, Loaded, Material, Vertex, face_normal, srgb_to_linear};

/// No index (a corner without `vt` or `vn`).
const NONE: u32 = u32::MAX;

pub fn parse(
    bytes: &[u8],
    stem: &str,
    sidecar: &dyn Fn(&str) -> Result<Vec<u8>, Error>,
    limits: &Limits,
) -> Result<Loaded, Error> {
    let text = String::from_utf8_lossy(bytes);
    let mut b = Builder::new(limits);
    let mut warnings = Vec::new();
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut shared: HashMap<(u32, u32, u32), u32> = HashMap::new();
    let mut materials: HashMap<String, u32> = HashMap::new();
    let mut images: HashMap<String, Option<u32>> = HashMap::new();
    let mut libraries_read: Vec<String> = Vec::new();
    let mut part_name = stem.to_string();
    let mut part: Option<u32> = None;
    let mut material = 0u32;
    let mut unknown_materials = 0usize;
    let mut skipped = 0usize;
    let mut corners: Vec<(u32, u32, u32)> = Vec::new();

    for (line_no, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        let Some((key, rest)) = split_key(line) else { continue };
        let at = || format!("line {}", line_no + 1);
        match key {
            "v" => {
                if positions.len() >= limits.max_vertices {
                    return Err(Error::new(format!("more than {} positions", limits.max_vertices)));
                }
                positions.push(floats3(rest).ok_or_else(|| Error::new(format!("{}: bad position", at())))?);
            }
            "vt" => {
                if uvs.len() >= limits.max_vertices {
                    return Err(Error::new(format!("more than {} texture coordinates", limits.max_vertices)));
                }
                let mut it = rest.split_whitespace().map(|t| t.parse::<f32>().ok());
                let u = it.next().flatten().ok_or_else(|| Error::new(format!("{}: bad `vt`", at())))?;
                let v = it.next().flatten().unwrap_or(0.0);
                uvs.push([u, 1.0 - v]);
            }
            "vn" => {
                if normals.len() >= limits.max_vertices {
                    return Err(Error::new(format!("more than {} normals", limits.max_vertices)));
                }
                normals.push(crate::normalize(
                    floats3(rest).ok_or_else(|| Error::new(format!("{}: bad normal", at())))?,
                ));
            }
            "o" | "g" => {
                let name = rest.trim();
                part_name = if name.is_empty() { stem.to_string() } else { name.to_string() };
                part = None;
            }
            "usemtl" => {
                let name = rest.trim();
                material = match materials.get(name) {
                    Some(m) => *m,
                    None => {
                        unknown_materials += 1;
                        0
                    }
                };
            }
            "mtllib" => {
                let name = rest.trim().to_string();
                if name.is_empty() || libraries_read.contains(&name) {
                    continue;
                }
                libraries_read.push(name.clone());
                match sidecar(&name) {
                    Ok(data) => {
                        read_mtl(&data, &name, &mut b, &mut materials, &mut images, sidecar, &mut warnings)?
                    }
                    Err(e) => warnings.push(format!("material library: {e}")),
                }
            }
            "f" => {
                corners.clear();
                for token in rest.split_whitespace() {
                    let c = corner(token, positions.len(), uvs.len(), normals.len())
                        .ok_or_else(|| Error::new(format!("{}: bad face corner `{token}`", at())))?;
                    corners.push(c);
                }
                if corners.len() < 3 {
                    skipped += 1;
                    continue;
                }
                let p = match part {
                    Some(p) => p,
                    None => {
                        let p = b.part(&part_name)?;
                        part = Some(p);
                        p
                    }
                };
                b.reserve_triangles(corners.len() - 2)?;
                let flat = corners.iter().any(|c| c.2 == NONE);
                let first = corners.first().copied().unwrap_or((0, NONE, NONE));
                for pair in corners.windows(2).skip(1) {
                    let tri = [
                        first,
                        pair.first().copied().unwrap_or(first),
                        pair.get(1).copied().unwrap_or(first),
                    ];
                    if flat {
                        let pos = tri.map(|c| positions.get(c.0 as usize).copied().unwrap_or_default());
                        let uv = tri.map(|c| uv_of(&uvs, c.1));
                        if face_normal(pos) == [0.0; 3] {
                            // Degenerate: nothing to see, nothing to shade.
                            continue;
                        }
                        b.flat_triangle(pos, uv, p, material)?;
                    } else {
                        let mut ids = [0u32; 3];
                        for (slot, c) in ids.iter_mut().zip(tri) {
                            *slot = match shared.get(&c) {
                                Some(i) => *i,
                                None => {
                                    let v = Vertex {
                                        position: positions.get(c.0 as usize).copied().unwrap_or_default(),
                                        normal: normals.get(c.2 as usize).copied().unwrap_or_default(),
                                        uv: uv_of(&uvs, c.1),
                                    };
                                    let i = b.vertex(v)?;
                                    shared.insert(c, i);
                                    i
                                }
                            };
                        }
                        b.triangle(ids, p, material)?;
                    }
                }
            }
            _ => {}
        }
    }
    if unknown_materials > 0 {
        warnings.push(format!("{unknown_materials} `usemtl` named a material no library defines"));
    }
    if skipped > 0 {
        warnings.push(format!("{skipped} faces with fewer than three corners skipped"));
    }
    if uvs.is_empty() {
        warnings.push("no texture coordinates: a texture shows as one colour".into());
    }
    Ok(Loaded { mesh: b.finish(), warnings })
}

fn uv_of(uvs: &[[f32; 2]], i: u32) -> [f32; 2] {
    if i == NONE { [0.0; 2] } else { uvs.get(i as usize).copied().unwrap_or_default() }
}

fn split_key(line: &str) -> Option<(&str, &str)> {
    if line.is_empty() {
        return None;
    }
    Some(line.split_once(char::is_whitespace).unwrap_or((line, "")))
}

fn floats3(rest: &str) -> Option<[f32; 3]> {
    let mut it = rest.split_whitespace().map(|t| t.parse::<f32>().ok());
    let x = it.next()??;
    let y = it.next()??;
    let z = it.next()??;
    [x, y, z].iter().all(|c| c.is_finite()).then_some([x, y, z])
}

/// `v`, `v/vt`, `v//vn` or `v/vt/vn`, 1-based or negative (counted back from the last), as
/// 0-based indices with [`NONE`] where absent. `None` for an index out of range.
fn corner(token: &str, nv: usize, nt: usize, nn: usize) -> Option<(u32, u32, u32)> {
    let mut parts = token.split('/');
    let v = index(parts.next()?, nv)?;
    let t = match parts.next() {
        None | Some("") => NONE,
        Some(s) => index(s, nt)?,
    };
    let n = match parts.next() {
        None | Some("") => NONE,
        Some(s) => index(s, nn)?,
    };
    Some((v, t, n))
}

fn index(s: &str, count: usize) -> Option<u32> {
    let i: i64 = s.parse().ok()?;
    let count = count as i64;
    let zero_based = if i > 0 { i - 1 } else { count + i };
    if i == 0 || zero_based < 0 || zero_based >= count {
        return None;
    }
    u32::try_from(zero_based).ok()
}

#[allow(clippy::too_many_arguments)]
fn read_mtl(
    data: &[u8],
    library: &str,
    b: &mut Builder<'_>,
    materials: &mut HashMap<String, u32>,
    images: &mut HashMap<String, Option<u32>>,
    sidecar: &dyn Fn(&str) -> Result<Vec<u8>, Error>,
    warnings: &mut Vec<String>,
) -> Result<(), Error> {
    let text = String::from_utf8_lossy(data);
    // The material being read: name, colour, texture.
    let mut current: Option<Material> = None;
    let mut finish = |m: Option<Material>, b: &mut Builder<'_>| -> Result<(), Error> {
        if let Some(m) = m {
            let name = m.name.clone();
            let id = b.material(m)?;
            materials.insert(name, id);
        }
        Ok(())
    };
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        let Some((key, rest)) = split_key(line) else { continue };
        match key {
            "newmtl" => {
                finish(current.take(), b)?;
                current = Some(Material::plain(rest.trim(), [0.8, 0.8, 0.8, 1.0]));
            }
            "Kd" => {
                if let (Some(m), Some(c)) = (current.as_mut(), floats3(rest)) {
                    let [r, g, bl] = c.map(|x| srgb_to_linear(x.clamp(0.0, 1.0)));
                    m.color = [r, g, bl, m.color[3]];
                }
            }
            "d" | "Tr" => {
                if let (Some(m), Some(Ok(a))) =
                    (current.as_mut(), rest.split_whitespace().next().map(str::parse::<f32>))
                    && a.is_finite()
                {
                    m.color[3] = if key == "d" { a } else { 1.0 - a }.clamp(0.0, 1.0);
                }
            }
            "map_Kd" => {
                let Some(m) = current.as_mut() else { continue };
                // Options (`-s 1 1 1`) come first; the file name is the last word.
                let Some(file) = rest.split_whitespace().last() else { continue };
                let id = match images.get(file) {
                    Some(id) => *id,
                    None => {
                        let id = match sidecar(file) {
                            Ok(bytes) => Some(b.image(Image { name: file.to_string(), bytes })?),
                            Err(e) => {
                                warnings.push(format!("{library}: texture {e}"));
                                None
                            }
                        };
                        images.insert(file.to_string(), id);
                        id
                    }
                };
                m.texture = id;
            }
            _ => {}
        }
    }
    finish(current.take(), b)
}
