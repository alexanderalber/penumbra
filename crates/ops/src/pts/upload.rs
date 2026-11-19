//! Points made on the CPU, uploaded: what the nodes that read meshes, channels, tables and files
//! share. Each attribute is a column of words in the layout its buffer has ([`Column`]); the
//! buffers come from one ring per name, so a node that uploads every cook reuses them.

use std::collections::HashMap;

use penumbra_core::Error;
use penumbra_gpu::points::{Attr, AttrType, Points, capacity_for, most_elements};
use penumbra_gpu::{Buffer, BufferRing, Gpu};
use penumbra_mesh::Bounds;

/// One attribute of points made on the CPU: its words, `ty.stride() / 4` per point (a `vec3f`
/// takes four, the fourth 1 for `position` and 0 otherwise).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Column {
    pub name: String,
    pub ty: AttrType,
    pub words: Vec<u32>,
}

impl Column {
    pub fn new(name: &str, ty: AttrType) -> Self {
        Self { name: name.to_string(), ty, words: Vec::new() }
    }

    /// Appends one point's value, in as many floats as the type has (whole numbers rounded).
    pub fn push(&mut self, v: [f32; 4]) {
        match self.ty {
            AttrType::F32 => self.words.push(v[0].to_bits()),
            AttrType::U32 => self.words.push(if v[0].is_finite() { v[0].round().max(0.0) as u32 } else { 0 }),
            AttrType::I32 => self.words.push(if v[0].is_finite() { v[0].round() as i32 as u32 } else { 0 }),
            AttrType::Vec2 => self.words.extend([v[0], v[1]].map(f32::to_bits)),
            AttrType::Vec3 => {
                let w = if self.name == "position" { 1.0 } else { 0.0 };
                self.words.extend([v[0], v[1], v[2], w].map(f32::to_bits));
            }
            AttrType::Vec4 => self.words.extend(v.map(f32::to_bits)),
        }
    }

    /// Points it holds.
    pub fn len(&self) -> usize {
        self.words.len() / (self.ty.stride() / 4) as usize
    }
}

/// The buffers of a node that uploads points, a ring per attribute name.
#[derive(Default)]
pub(crate) struct Uploads {
    rings: HashMap<String, BufferRing>,
}

impl Uploads {
    /// `count` points with `columns` as their attributes (each `count` long) and `bounds`.
    /// `Err` for a column of another length, two of one name, a name that is no identifier.
    pub fn upload(
        &mut self,
        gpu: &Gpu,
        count: usize,
        columns: &[Column],
        bounds: Option<Bounds>,
    ) -> Result<Points, Error> {
        let n = u32::try_from(count).map_err(|_| Error::new("too many points"))?;
        self.rings.retain(|name, _| columns.iter().any(|c| c.name == *name));
        let capacity = capacity_for(n, most_elements(gpu, AttrType::Vec4)).max(1);
        let mut attrs = Vec::with_capacity(columns.len());
        for c in columns {
            if c.len() != count {
                return Err(Error::new(format!(
                    "the attribute `{}` has {} values for {count} points",
                    c.name,
                    c.len()
                )));
            }
            let ring = self.rings.entry(c.name.clone()).or_default();
            let buffer: Buffer =
                ring.next(gpu, u64::from(capacity) * c.ty.stride(), &format!("pts {}", c.name))?;
            buffer.write(gpu, 0, &c.words)?;
            attrs.push(Attr::new(&c.name, c.ty, buffer)?);
        }
        Ok(Points::new(capacity, n, attrs)?.with_bounds(bounds.filter(|_| n > 0)))
    }

    pub fn bytes(&self) -> u64 {
        self.rings.values().map(BufferRing::bytes).sum()
    }

    pub fn trim(&mut self) {
        for r in self.rings.values_mut() {
            r.trim();
        }
    }
}

/// The box around the positions in `column` (four words a point), `None` without any.
pub(crate) fn bounds_of(column: &Column) -> Option<Bounds> {
    let f = |w: u32| f32::from_bits(w);
    Bounds::of(
        column
            .words
            .as_chunks::<4>()
            .0
            .iter()
            .map(|[x, y, z, _]| [f(*x), f(*y), f(*z)])
            .filter(|p| p.iter().all(|x| x.is_finite())),
    )
}
