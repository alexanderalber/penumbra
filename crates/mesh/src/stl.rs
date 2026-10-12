//! STL, binary and ASCII. One part named after the file (separate files make separate parts
//! through `geo.merge`). The file's facet normals are ignored (often wrong); every triangle gets
//! its own flat normal from its corners.

use crate::{Builder, Error, Limits, Loaded, face_normal};

/// 80-byte header, a triangle count, 50 bytes per triangle.
pub fn is_binary(bytes: &[u8]) -> bool {
    count(bytes).is_some_and(|n| 84u64.checked_add(n.saturating_mul(50)) == Some(bytes.len() as u64))
}

fn count(bytes: &[u8]) -> Option<u64> {
    let b: [u8; 4] = bytes.get(80..84)?.try_into().ok()?;
    Some(u64::from(u32::from_le_bytes(b)))
}

pub fn parse(bytes: &[u8], stem: &str, limits: &Limits) -> Result<Loaded, Error> {
    let mut b = Builder::new(limits);
    let part = b.part(stem)?;
    if is_binary(bytes) {
        let n = count(bytes).unwrap_or(0) as usize;
        b.reserve_triangles(n)?;
        b.reserve_vertices(n.saturating_mul(3))?;
        let body = bytes.get(84..).unwrap_or(&[]);
        for record in body.as_chunks::<50>().0 {
            let f = |k: usize| {
                record.get(k..k + 4).and_then(|s| s.try_into().ok()).map_or(f32::NAN, f32::from_le_bytes)
            };
            // 12 bytes normal, then three corners.
            let p = [0, 1, 2].map(|c| [f(12 + c * 12), f(16 + c * 12), f(20 + c * 12)]);
            if face_normal(p) == [0.0; 3] && p.iter().flatten().all(|c| c.is_finite()) {
                // Degenerate (common at the poles of exported spheres): draws nothing.
                continue;
            }
            b.flat_triangle(p, [[0.0; 2]; 3], part, 0)?;
        }
    } else {
        let text = String::from_utf8_lossy(bytes);
        if !text.trim_start().starts_with("solid") {
            return Err(Error::new("neither binary STL (the size does not match the count) nor ASCII STL"));
        }
        let mut corner: Vec<[f32; 3]> = Vec::with_capacity(3);
        for (line_no, raw) in text.lines().enumerate() {
            let mut words = raw.split_whitespace();
            match words.next() {
                Some("vertex") => {
                    let mut c = [0.0f32; 3];
                    for slot in &mut c {
                        *slot = words
                            .next()
                            .and_then(|w| w.parse().ok())
                            .ok_or_else(|| Error::new(format!("line {}: bad vertex", line_no + 1)))?;
                    }
                    if corner.len() >= 3 {
                        return Err(Error::new(format!(
                            "line {}: more than three corners in a facet",
                            line_no + 1
                        )));
                    }
                    corner.push(c);
                }
                Some("endfacet") => {
                    let p: Option<[[f32; 3]; 3]> = corner.as_slice().try_into().ok();
                    corner.clear();
                    match p {
                        Some(p) if face_normal(p) != [0.0; 3] => {
                            b.flat_triangle(p, [[0.0; 2]; 3], part, 0)?
                        }
                        Some(p) if p.iter().flatten().all(|c| c.is_finite()) => {} // degenerate: draws nothing
                        Some(_) => return Err(Error::new(format!("line {}: not finite", line_no + 1))),
                        None => {
                            return Err(Error::new(format!(
                                "line {}: a facet without three corners",
                                line_no + 1
                            )));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    Ok(Loaded { mesh: b.finish(), warnings: Vec::new() })
}
