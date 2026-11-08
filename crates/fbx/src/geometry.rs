//! A `Geometry` object of sub-class `Mesh`: control points, polygons and their layer elements.
//!
//! `Vertices` holds the control points (x, y, z each), `PolygonVertexIndex` one control point
//! per polygon corner, the last corner of each polygon stored as `-i - 1`. The layer elements
//! (`LayerElementNormal`, `LayerElementUV`, `LayerElementMaterial`) carry a mapping: per corner
//! (`ByPolygonVertex`), per control point (`ByVertex`, also spelled `ByVertice`), per polygon
//! (`ByPolygon`) or one for all (`AllSame`). With an index array (`NormalsIndex`, `UVIndex`)
//! the values are looked up through it, without one they are taken in order. Some old writers
//! store `ByPolygon` where they mean per corner; when the count fits the corners, it is read so.
//!
//! Polygons with fewer than three corners, or with a corner outside the control points, are
//! skipped and counted. A last polygon without its negative end index is closed anyway.

use crate::tree::{Array, Node, Value};
use crate::{Error, Limits};

/// No value: an index that pointed outside its array.
pub const NONE: u32 = u32::MAX;

/// A polygon's corners: `count` entries of [`Geometry::corners`] from `first`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Polygon {
    /// The polygon's first corner.
    pub first: u32,
    /// How many corners it has (at least 3).
    pub count: u32,
    /// The polygon's position in the file, counting the skipped ones too (what `ByPolygon`
    /// layers refer to).
    pub index: u32,
}

/// Values and, per polygon corner, which value it takes ([`NONE`] for none).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Attribute<T> {
    /// The values as stored.
    pub values: Vec<T>,
    /// Per polygon corner, the index into `values`.
    pub index: Vec<u32>,
}

impl<T: Copy> Attribute<T> {
    /// The value at polygon corner `corner`.
    pub fn at(&self, corner: usize) -> Option<T> {
        let i = *self.index.get(corner)?;
        self.values.get(i as usize).copied()
    }
}

/// A mesh geometry as stored, before any transform.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Geometry {
    /// The control points as stored, in the file's units.
    pub positions: Vec<[f64; 3]>,
    /// Per polygon corner, the control point.
    pub corners: Vec<u32>,
    /// The usable polygons, in file order.
    pub polygons: Vec<Polygon>,
    /// The first normal layer, per corner.
    pub normals: Option<Attribute<[f64; 3]>>,
    /// The first UV set (the lowest layer index), origin at the bottom left as in the file.
    pub uvs: Option<Attribute<[f64; 2]>>,
    /// The name of that UV set.
    pub uv_set: String,
    /// Per entry of `polygons`, the index into the model's materials (in connection order).
    /// Empty when the file has no material layer: everything uses material 0.
    pub materials: Vec<u32>,
    /// Polygons skipped: fewer than three corners.
    pub degenerate: usize,
    /// Polygons skipped: a corner outside the control points.
    pub out_of_range: usize,
    /// Layer elements whose mapping was not understood (they are left out).
    pub unknown_mappings: Vec<String>,
}

fn mapping(layer: &Node) -> &str {
    layer.child_str("MappingInformationType").unwrap_or("")
}

impl Geometry {
    /// Reads a `Geometry` record.
    pub fn read(node: &Node, limits: &Limits) -> Result<Geometry, Error> {
        let mut g = Geometry::default();
        let Some(vertices) = node.child_array("Vertices") else { return Ok(g) };
        let count = vertices.len() / 3;
        if count > limits.max_vertices {
            return Err(Error::new(format!("more than {} control points", limits.max_vertices)));
        }
        g.positions = (0..count)
            .map(|i| {
                let c = |k: usize| vertices.f64_at(i * 3 + k).filter(|x| x.is_finite()).unwrap_or(0.0);
                [c(0), c(1), c(2)]
            })
            .collect();
        let Some(index) = node.child_array("PolygonVertexIndex") else { return Ok(g) };
        if index.len() > limits.max_polygon_vertices {
            return Err(Error::new(format!("more than {} polygon corners", limits.max_polygon_vertices)));
        }
        g.corners = Vec::with_capacity(index.len());
        let mut first = 0usize;
        let mut file_polygon = 0u32;
        let mut bad = false;
        for k in 0..index.len() {
            let raw = index.i64_at(k).unwrap_or(0);
            let end = raw < 0 || k + 1 == index.len();
            let v = if raw < 0 { -(raw + 1) } else { raw };
            let v = u32::try_from(v).unwrap_or(NONE);
            if v as usize >= count {
                bad = true;
            }
            g.corners.push(v);
            if end {
                let n = k + 1 - first;
                if bad {
                    g.out_of_range += 1;
                } else if n < 3 {
                    g.degenerate += 1;
                } else {
                    g.polygons.push(Polygon { first: first as u32, count: n as u32, index: file_polygon });
                }
                file_polygon = file_polygon.saturating_add(1);
                first = k + 1;
                bad = false;
            }
        }
        let polygons_in_file = file_polygon as usize;
        // Per corner, its polygon's position in the file (for `ByPolygon`).
        let polygon_of = |corner: usize, polys: &[Polygon]| -> Option<usize> {
            let at = polys.partition_point(|p| (p.first as usize) <= corner);
            let p = polys.get(at.checked_sub(1)?)?;
            ((corner - p.first as usize) < p.count as usize).then_some(p.index as usize)
        };
        if let Some(layer) = node.children_named("LayerElementNormal").next() {
            g.normals =
                g.attribute(layer, "Normals", "NormalsIndex", 3, polygons_in_file, &polygon_of, |a, i| {
                    Some([0, 1, 2].map(|k| a.f64_at(i * 3 + k).filter(|x| x.is_finite()).unwrap_or(0.0)))
                });
        }
        let uv_layer = node
            .children_named("LayerElementUV")
            .min_by_key(|l| l.value(0).and_then(Value::as_i64).unwrap_or(0));
        if let Some(layer) = uv_layer {
            g.uv_set = layer.child_str("Name").unwrap_or("").to_string();
            g.uvs = g.attribute(layer, "UV", "UVIndex", 2, polygons_in_file, &polygon_of, |a, i| {
                let u = a.f64_at(i * 2)?;
                let v = a.f64_at(i * 2 + 1)?;
                Some([u, v].map(|x| if x.is_finite() { x } else { 0.0 }))
            });
        }
        if let Some(layer) = node.children_named("LayerElementMaterial").next() {
            let list = layer.child_array("Materials");
            let at =
                |i: usize| list.and_then(|l| l.i64_at(i)).and_then(|m| u32::try_from(m).ok()).unwrap_or(0);
            match mapping(layer) {
                "AllSame" => g.materials = vec![at(0); g.polygons.len()],
                "ByPolygon" => g.materials = g.polygons.iter().map(|p| at(p.index as usize)).collect(),
                "" => {}
                other => g.unknown_mappings.push(format!("LayerElementMaterial: {other}")),
            }
        }
        Ok(g)
    }

    /// Resolves one layer element to a value index per corner.
    #[allow(clippy::too_many_arguments)]
    fn attribute<T>(
        &mut self,
        layer: &Node,
        data: &str,
        index: &str,
        width: usize,
        polygons_in_file: usize,
        polygon_of: &dyn Fn(usize, &[Polygon]) -> Option<usize>,
        value: impl Fn(&Array, usize) -> Option<T>,
    ) -> Option<Attribute<T>> {
        let array = layer.child_array(data)?;
        let n = array.len() / width;
        if n == 0 {
            return None;
        }
        let values: Vec<T> = (0..n).filter_map(|i| value(array, i)).collect();
        let lookup = layer.child_array(index);
        let through = |i: usize| -> u32 {
            let i = match lookup {
                Some(l) => match l.i64_at(i).and_then(|x| usize::try_from(x).ok()) {
                    Some(x) => x,
                    None => return NONE,
                },
                None => i,
            };
            if i < n { i as u32 } else { NONE }
        };
        let corners = self.corners.len();
        let mut map = mapping(layer).to_string();
        let entries = lookup.map_or(n, Array::len);
        if map == "ByPolygon" && entries == corners && entries != polygons_in_file {
            map = "ByPolygonVertex".into();
        }
        let index: Vec<u32> = match map.as_str() {
            "ByPolygonVertex" => (0..corners).map(through).collect(),
            "ByVertex" | "ByVertice" => {
                self.corners.iter().map(|v| if *v == NONE { NONE } else { through(*v as usize) }).collect()
            }
            "ByPolygon" => {
                (0..corners).map(|c| polygon_of(c, &self.polygons).map_or(NONE, through)).collect()
            }
            "AllSame" => vec![through(0); corners],
            other => {
                self.unknown_mappings.push(format!("{}: {other}", layer.name));
                return None;
            }
        };
        Some(Attribute { values, index })
    }

    /// The control point of corner `corner`.
    pub fn position(&self, corner: usize) -> Option<[f64; 3]> {
        self.positions.get(*self.corners.get(corner)? as usize).copied()
    }

    /// The material slot of entry `i` of `polygons`.
    pub fn material(&self, i: usize) -> u32 {
        self.materials.get(i).copied().unwrap_or(0)
    }

    /// The triangles of a polygon as corner indices (into [`Geometry::corners`]), keeping its
    /// winding. Ear clipping in the polygon's plane, so concave polygons come out right; a
    /// polygon it cannot clip (self-intersecting, or of more than 256 corners) is split as a
    /// fan.
    pub fn triangulate(&self, p: &Polygon) -> Vec<[u32; 3]> {
        let mut unlimited = u64::MAX;
        self.triangulate_within(p, &mut unlimited)
    }

    /// Like [`Geometry::triangulate`], taking the clipping's work from `work` (steps of the
    /// inside test). Ear clipping takes up to the cube of the corners; a file of many large
    /// polygons could tie up the reader for minutes. Once `work` runs out, the rest are split
    /// as fans. [`CLIP_WORK`] is a budget for one file.
    pub fn triangulate_within(&self, p: &Polygon, work: &mut u64) -> Vec<[u32; 3]> {
        let first = p.first as usize;
        let n = p.count as usize;
        let points: Vec<[f64; 3]> =
            (first..first + n).map(|c| self.position(c).unwrap_or([0.0; 3])).collect();
        triangulate_within(&points, work).into_iter().map(|t| t.map(|k| (first + k) as u32)).collect()
    }
}

/// Steps of ear clipping one file may take, a second or two of work; polygons after that are
/// split as fans.
pub const CLIP_WORK: u64 = 400_000_000;

/// Triangles over the corners of a planar-ish polygon (indices into `points`).
#[cfg(test)]
pub(crate) fn triangulate(points: &[[f64; 3]]) -> Vec<[usize; 3]> {
    let mut unlimited = u64::MAX;
    triangulate_within(points, &mut unlimited)
}

/// Triangles over the corners of a planar-ish polygon (indices into `points`), the clipping
/// taking its steps from `work`.
pub(crate) fn triangulate_within(points: &[[f64; 3]], work: &mut u64) -> Vec<[usize; 3]> {
    let n = points.len();
    let fan = || (1..n.saturating_sub(1)).map(|k| [0, k, k + 1]).collect::<Vec<_>>();
    if n < 3 {
        return Vec::new();
    }
    if n == 3 || n > 256 || *work == 0 {
        return fan();
    }
    // Newell's normal, then the plane it is most perpendicular to.
    let mut nrm = [0.0f64; 3];
    for (i, a) in points.iter().enumerate() {
        let b = points.get((i + 1) % n).copied().unwrap_or(*a);
        nrm[0] += (a[1] - b[1]) * (a[2] + b[2]);
        nrm[1] += (a[2] - b[2]) * (a[0] + b[0]);
        nrm[2] += (a[0] - b[0]) * (a[1] + b[1]);
    }
    let [ax, ay, az] = nrm.map(f64::abs);
    if !(ax + ay + az).is_finite() || ax + ay + az <= 0.0 {
        return fan();
    }
    // Project so that the polygon runs counter-clockwise in 2D.
    let flat: Vec<[f64; 2]> = points
        .iter()
        .map(|p| {
            if az >= ax && az >= ay {
                if nrm[2] >= 0.0 { [p[0], p[1]] } else { [p[1], p[0]] }
            } else if ax >= ay {
                if nrm[0] >= 0.0 { [p[1], p[2]] } else { [p[2], p[1]] }
            } else if nrm[1] >= 0.0 {
                [p[2], p[0]]
            } else {
                [p[0], p[2]]
            }
        })
        .collect();
    let at = |i: usize| flat.get(i).copied().unwrap_or([0.0; 2]);
    let cross =
        |o: [f64; 2], a: [f64; 2], b: [f64; 2]| (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0]);
    let mut left: Vec<usize> = (0..n).collect();
    let mut out = Vec::with_capacity(n - 2);
    let mut guard = 0usize;
    while left.len() > 3 {
        guard += 1;
        if guard > n * n {
            break;
        }
        let m = left.len();
        let mut clipped = false;
        for k in 0..m {
            // `as`: at most 256 corners.
            let Some(rest) = work.checked_sub(m as u64) else {
                *work = 0;
                break;
            };
            *work = rest;
            let (Some(&i0), Some(&i1), Some(&i2)) =
                (left.get((k + m - 1) % m), left.get(k), left.get((k + 1) % m))
            else {
                continue;
            };
            let (a, b, c) = (at(i0), at(i1), at(i2));
            if cross(a, b, c) <= 0.0 {
                continue;
            }
            let inside = left.iter().any(|&j| {
                if j == i0 || j == i1 || j == i2 {
                    return false;
                }
                let p = at(j);
                cross(a, b, p) >= 0.0 && cross(b, c, p) >= 0.0 && cross(c, a, p) >= 0.0
            });
            if inside {
                continue;
            }
            out.push([i0, i1, i2]);
            left.remove(k);
            clipped = true;
            break;
        }
        if !clipped {
            break;
        }
    }
    if left.len() > 3 {
        // Not clippable: a fan over what is left.
        if let Some((&f, rest)) = left.split_first() {
            for w in rest.windows(2) {
                if let [b, c] = *w {
                    out.push([f, b, c]);
                }
            }
        }
    } else if let [a, b, c] = *left.as_slice() {
        out.push([a, b, c]);
    }
    out
}
