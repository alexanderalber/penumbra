//! Points on the GPU, as they flow between `pts` nodes ([`Points`]).
//!
//! A set of points is a list of attributes: named arrays with one element per point, each in a
//! storage buffer of its own ([`Attr`]). `position` is where a point is; `normal`, `color`,
//! `scale`, `uv` and the others are optional. A node that changes one attribute makes one new
//! buffer and passes the others on as they are (clones of the same buffers). No node writes into
//! its input's buffers: each owns its outputs, reused across cooks through a
//! [`crate::BufferRing`].
//!
//! Attribute names are penumbra's words ([`STANDARD`]). Wherever a user types one, the
//! TouchDesigner name means the same attribute ([`ALIASES`], [`canonical`]): `P` is `position`,
//! `Color` is `color`, `PointScale` is `scale`.
//!
//! Every buffer holds [`Points::capacity`] elements; the first [`Points::count`] are points. A
//! compute program runs over the capacity and returns for an index at or past the count. A
//! missing attribute reads as its default ([`default_of`]): position 0, normal +z, colour white,
//! scale 1.
//!
//! Layout in the buffers, as WGSL's `array<T>` has it: `f32`, `vec2f` (8 bytes), `vec3f` (16
//! bytes, the fourth float unused), `vec4f`, `u32`, `i32`.

use std::sync::Arc;

use penumbra_core::{Error, ParamSpec, Value};
use penumbra_mesh::Bounds;

use crate::{Buffer, BufferRing, Dispatch, FLOAT_FORMAT, Gpu, Out, Step, Tex, TexRing};

/// The type of one element of an attribute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AttrType {
    F32,
    Vec2,
    /// Three floats in 16 bytes, as `array<vec3f>` lays them out.
    Vec3,
    Vec4,
    U32,
    I32,
}

impl AttrType {
    /// Bytes per element in the buffer.
    pub fn stride(self) -> u64 {
        match self {
            AttrType::F32 | AttrType::U32 | AttrType::I32 => 4,
            AttrType::Vec2 => 8,
            AttrType::Vec3 | AttrType::Vec4 => 16,
        }
    }

    /// The WGSL type of one element.
    pub fn wgsl(self) -> &'static str {
        match self {
            AttrType::F32 => "f32",
            AttrType::Vec2 => "vec2f",
            AttrType::Vec3 => "vec3f",
            AttrType::Vec4 => "vec4f",
            AttrType::U32 => "u32",
            AttrType::I32 => "i32",
        }
    }

    /// Numbers per element.
    pub fn components(self) -> u32 {
        match self {
            AttrType::F32 | AttrType::U32 | AttrType::I32 => 1,
            AttrType::Vec2 => 2,
            AttrType::Vec3 => 3,
            AttrType::Vec4 => 4,
        }
    }
}

/// An attribute penumbra knows by name: its type and the value a point without it has.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Standard {
    pub name: &'static str,
    pub ty: AttrType,
    /// The value of a missing attribute, in as many components as the type has.
    pub default: [f32; 4],
}

/// The attributes penumbra knows by name. Others may exist under any name a node gives them.
pub const STANDARD: &[Standard] = &[
    Standard { name: "position", ty: AttrType::Vec3, default: [0.0, 0.0, 0.0, 0.0] },
    Standard { name: "normal", ty: AttrType::Vec3, default: [0.0, 0.0, 1.0, 0.0] },
    Standard { name: "color", ty: AttrType::Vec4, default: [1.0, 1.0, 1.0, 1.0] },
    Standard { name: "scale", ty: AttrType::F32, default: [1.0, 0.0, 0.0, 0.0] },
    Standard { name: "uv", ty: AttrType::Vec2, default: [0.0, 0.0, 0.0, 0.0] },
    Standard { name: "velocity", ty: AttrType::Vec3, default: [0.0; 4] },
    Standard { name: "force", ty: AttrType::Vec3, default: [0.0; 4] },
    Standard { name: "age", ty: AttrType::F32, default: [0.0; 4] },
    Standard { name: "life", ty: AttrType::F32, default: [0.0; 4] },
    Standard { name: "id", ty: AttrType::U32, default: [0.0; 4] },
    Standard { name: "mass", ty: AttrType::F32, default: [1.0, 0.0, 0.0, 0.0] },
    Standard { name: "drag", ty: AttrType::F32, default: [0.0; 4] },
    Standard { name: "weight", ty: AttrType::F32, default: [1.0, 0.0, 0.0, 0.0] },
];

/// TouchDesigner's attribute names and the penumbra names they stand for.
pub const ALIASES: &[(&str, &str)] = &[
    ("P", "position"),
    ("N", "normal"),
    ("Color", "color"),
    ("PointScale", "scale"),
    ("Tex", "uv"),
    ("PartVel", "velocity"),
    ("PartForce", "force"),
    ("PartAge", "age"),
    ("PartLifeSpan", "life"),
    ("PartId", "id"),
    ("PartMass", "mass"),
    ("PartDrag", "drag"),
    ("Weight", "weight"),
];

/// The penumbra name of an attribute a user typed: a TouchDesigner name becomes the attribute
/// it means, anything else stays as it is (without surrounding spaces).
pub fn canonical(name: &str) -> &str {
    let name = name.trim();
    ALIASES.iter().find(|(td, _)| *td == name).map_or(name, |(_, ours)| ours)
}

/// What a point without attribute `name` has: its type and value. `None` for a name penumbra
/// does not know.
pub fn default_of(name: &str) -> Option<&'static Standard> {
    let name = canonical(name);
    STANDARD.iter().find(|s| s.name == name)
}

/// Longest attribute name.
const MAX_NAME: usize = 64;

/// Most attributes one set of points holds.
pub const MAX_ATTRS: usize = 32;

/// Whether `name` can name an attribute: letters, digits and `_`, not starting with a digit,
/// so a compute program can use it as an identifier.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.starts_with(|c: char| c.is_ascii_digit())
}

/// One attribute: a name, a type, and a buffer with an element per point.
#[derive(Clone)]
pub struct Attr {
    name: Arc<str>,
    ty: AttrType,
    buffer: Buffer,
}

impl Attr {
    /// `buffer` as the attribute `name` (a TouchDesigner name is turned into penumbra's).
    /// `Err` for a name that is no identifier.
    pub fn new(name: &str, ty: AttrType, buffer: Buffer) -> Result<Self, Error> {
        let name = canonical(name);
        if !valid_name(name) {
            return Err(Error::new(format!(
                "`{name}` cannot name an attribute: letters, digits and _ only, up to {MAX_NAME}, not starting with a digit"
            )));
        }
        Ok(Self { name: name.into(), ty, buffer })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn ty(&self) -> AttrType {
        self.ty
    }

    pub fn buffer(&self) -> &Buffer {
        &self.buffer
    }

    /// Elements the buffer holds.
    pub fn len(&self) -> u64 {
        self.buffer.bytes() / self.ty.stride()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// How many of the elements are points.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Count {
    /// Known on the CPU, as a generator knows it.
    Known(u32),
}

impl Count {
    /// The count where the CPU knows it.
    pub fn known(self) -> Option<u32> {
        match self {
            Count::Known(n) => Some(n),
        }
    }

    /// No more than this many: the count itself where known.
    pub fn at_most(self) -> u32 {
        match self {
            Count::Known(n) => n,
        }
    }
}

/// How points join into lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Strips {
    /// They do not: each point stands alone.
    #[default]
    None,
    /// Every `len` points in a row are one line, in order.
    Regular { len: u32 },
}

/// Points on the GPU (`pts` data). Cheap to clone; clones share the buffers.
#[derive(Clone)]
pub struct Points {
    attrs: Arc<[Attr]>,
    capacity: u32,
    count: Count,
    bounds: Option<Bounds>,
    dims: Option<[u32; 3]>,
    strips: Strips,
}

impl Points {
    /// `count` points in `attrs`, whose buffers hold at least `capacity` elements each. `Err`
    /// for a count above the capacity, a buffer too small, two attributes of one name or more
    /// than [`MAX_ATTRS`].
    pub fn new(capacity: u32, count: u32, attrs: Vec<Attr>) -> Result<Self, Error> {
        if count > capacity {
            return Err(Error::new(format!("{count} points do not fit a capacity of {capacity}")));
        }
        check_attrs(capacity, &attrs)?;
        Ok(Self {
            attrs: attrs.into(),
            capacity,
            count: Count::Known(count),
            bounds: None,
            dims: None,
            strips: Strips::None,
        })
    }

    /// No points and no attributes.
    pub fn empty() -> Self {
        Self {
            attrs: Arc::from(Vec::new()),
            capacity: 0,
            count: Count::Known(0),
            bounds: None,
            dims: None,
            strips: Strips::None,
        }
    }

    pub fn attrs(&self) -> &[Attr] {
        &self.attrs
    }

    /// The attribute `name` (or the one a TouchDesigner name means), if the points have it.
    pub fn attr(&self, name: &str) -> Option<&Attr> {
        let name = canonical(name);
        self.attrs.iter().find(|a| a.name() == name)
    }

    /// Elements each attribute's buffer holds.
    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    pub fn count(&self) -> Count {
        self.count
    }

    /// The box around every position, where the node that made them knows it.
    pub fn bounds(&self) -> Option<Bounds> {
        self.bounds
    }

    /// Rows, columns and slices of an ordered set (a grid, a lattice); `None` when unordered.
    pub fn dims(&self) -> Option<[u32; 3]> {
        self.dims
    }

    pub fn strips(&self) -> Strips {
        self.strips
    }

    /// These points with `attr` added, or in place of the attribute of its name. `Err` when
    /// its buffer holds fewer elements than the capacity, or the points hold [`MAX_ATTRS`]
    /// already.
    pub fn with_attr(&self, attr: Attr) -> Result<Self, Error> {
        let mut attrs: Vec<Attr> = self.attrs.iter().filter(|a| a.name() != attr.name()).cloned().collect();
        attrs.push(attr);
        check_attrs(self.capacity, &attrs)?;
        Ok(Self { attrs: attrs.into(), ..self.clone() })
    }

    /// These points without the attribute `name`.
    pub fn without(&self, name: &str) -> Self {
        let name = canonical(name);
        let attrs: Vec<Attr> = self.attrs.iter().filter(|a| a.name() != name).cloned().collect();
        Self { attrs: attrs.into(), ..self.clone() }
    }

    pub fn with_bounds(self, bounds: Option<Bounds>) -> Self {
        Self { bounds, ..self }
    }

    /// With `dims`; `None` where the product of the dims is not the count.
    pub fn with_dims(self, dims: Option<[u32; 3]>) -> Self {
        let fits = |d: [u32; 3]| {
            d.iter().try_fold(1u32, |a, b| a.checked_mul(*b)) == self.count.known().filter(|n| *n > 0)
        };
        Self { dims: dims.filter(|d| fits(*d)), ..self }
    }

    pub fn with_strips(self, strips: Strips) -> Self {
        Self { strips, ..self }
    }

    /// Bytes on the GPU, each buffer once.
    pub fn bytes(&self) -> u64 {
        crate::buffer::unique_buffer_bytes(self.attrs.iter().map(Attr::buffer))
    }

    /// The attribute `name` as a picture of [`FLOAT_FORMAT`], point `i` at texel
    /// `(i % width, i / width)` ([`texel_size`]), for drawing and for nodes that read
    /// pictures. A scalar becomes grey (`(v, v, v, 1)`), a `vec2f` red and green with alpha 1,
    /// a `vec3f` rgb with whatever the fourth float holds as alpha. Texels past the count hold
    /// nothing in particular. `None` where the points do not have the attribute or have no
    /// points. `textures` keeps the node's pictures across cooks, `scratch` the buffers a
    /// conversion needs.
    pub fn attr_texture(
        &self,
        gpu: &Gpu,
        name: &str,
        textures: &mut TexRing,
        scratch: &mut BufferRing,
    ) -> Result<Option<Tex>, Error> {
        let count = self.count.at_most();
        let Some(attr) = self.attr(name) else { return Ok(None) };
        if count == 0 {
            return Ok(None);
        }
        let (w, h) = texel_size(count);
        let tex = textures.next_in(gpu, w, h, FLOAT_FORMAT, &format!("pts {}", attr.name()))?;
        let texels = match attr.ty() {
            AttrType::Vec3 | AttrType::Vec4 => attr.buffer().clone(),
            ty => {
                let out = scratch.next(gpu, u64::from(count) * 16, "pts texels")?;
                expand(gpu, attr.buffer(), ty, count, (w, h), &out)?;
                out
            }
        };
        copy_rows(gpu, &texels, count, &tex)?;
        Ok(Some(tex))
    }
}

/// Points per row of the pictures [`Points::attr_texture`] makes.
pub const TEXEL_ROW: u32 = 1024;

/// The size of the picture that holds `count` points: one row of up to [`TEXEL_ROW`], a
/// multiple of 16 wide (rows of a copy start on 256 bytes), as many rows as needed.
pub fn texel_size(count: u32) -> (u32, u32) {
    let count = count.max(1);
    let w = count.next_multiple_of(16).min(TEXEL_ROW);
    (w, count.div_ceil(w))
}

fn check_attrs(capacity: u32, attrs: &[Attr]) -> Result<(), Error> {
    if attrs.len() > MAX_ATTRS {
        return Err(Error::new(format!(
            "{} attributes are more than the {MAX_ATTRS} points hold",
            attrs.len()
        )));
    }
    for (k, a) in attrs.iter().enumerate() {
        if a.len() < u64::from(capacity) {
            return Err(Error::new(format!(
                "the attribute `{}` holds {} elements, fewer than the capacity of {capacity}",
                a.name(),
                a.len()
            )));
        }
        if attrs.iter().skip(k + 1).any(|b| b.name() == a.name()) {
            return Err(Error::new(format!("two attributes are named `{}`", a.name())));
        }
    }
    Ok(())
}

/// Turns elements of 4 or 8 bytes into one `vec4f` each, for a copy into a picture.
const EXPAND: &str = "
@group(2) @binding(0) var<storage, read> words: array<u32>;
@group(2) @binding(1) var<storage, read_write> texels: array<vec4f>;

@compute @workgroup_size(64)
fn expand(@builtin(global_invocation_id) id: vec3u) {
    let i = id.y * pn.size.x + id.x;
    if id.x >= pn.size.x || id.y >= pn.size.y || i >= u32(params.count) {
        return;
    }
    var v = vec4f(0.0, 0.0, 0.0, 1.0);
    switch params.kind {
        case 0: {
            let s = bitcast<f32>(words[i]);
            v = vec4f(s, s, s, 1.0);
        }
        case 1: {
            v = vec4f(bitcast<f32>(words[2u * i]), bitcast<f32>(words[2u * i + 1u]), 0.0, 1.0);
        }
        case 2: {
            let s = f32(words[i]);
            v = vec4f(s, s, s, 1.0);
        }
        case 3: {
            let s = f32(bitcast<i32>(words[i]));
            v = vec4f(s, s, s, 1.0);
        }
        default: {}
    }
    texels[i] = v;
}
";

fn expand_params() -> [ParamSpec; 2] {
    [ParamSpec::int("kind", 0), ParamSpec::int("count", 0)]
}

/// `count` elements of `src` of type `ty` (4 or 8 bytes each) into `vec4f`s in `out`.
fn expand(
    gpu: &Gpu,
    src: &Buffer,
    ty: AttrType,
    count: u32,
    (w, h): (u32, u32),
    out: &Buffer,
) -> Result<(), Error> {
    let kind = match ty {
        AttrType::F32 => 0,
        AttrType::Vec2 => 1,
        AttrType::U32 => 2,
        AttrType::I32 => 3,
        AttrType::Vec3 | AttrType::Vec4 => return Err(Error::new("a vec3f or vec4f needs no expansion")),
    };
    let program = gpu.cached_compute("pts texels", EXPAND, &expand_params())?;
    let param = |n: &str| match n {
        "kind" => Some(Value::Int(kind)),
        "count" => Some(Value::Int(i64::from(count))),
        _ => None,
    };
    let input = Dispatch { volumes: &[], pictures: &[], data: None, seconds: 0.0, frame: 0, param: &param };
    program.run_with(gpu, &[src, out], &[Step { entry: "expand", out: Out::grid(w, h), input: &input }])
}

/// Copies `count` elements of 16 bytes from `src` into the texels of `tex`, row by row.
fn copy_rows(gpu: &Gpu, src: &Buffer, count: u32, tex: &Tex) -> Result<(), Error> {
    let (w, h) = tex.size();
    if u64::from(w) * u64::from(h) < u64::from(count) || src.bytes() < u64::from(count) * 16 {
        return Err(Error::new("the points do not fit the picture"));
    }
    let full = count / w;
    let rest = count % w;
    let ((), error) = gpu.scoped(|| {
        let mut encoder = gpu
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("pts texels") });
        let at = |y: u32| wgpu::TexelCopyTextureInfo {
            texture: tex.texture(),
            mip_level: 0,
            origin: wgpu::Origin3d { x: 0, y, z: 0 },
            aspect: wgpu::TextureAspect::All,
        };
        if full > 0 {
            encoder.copy_buffer_to_texture(
                wgpu::TexelCopyBufferInfo {
                    buffer: src.buffer(),
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(w * 16),
                        rows_per_image: None,
                    },
                },
                at(0),
                wgpu::Extent3d { width: w, height: full, depth_or_array_layers: 1 },
            );
        }
        if rest > 0 {
            encoder.copy_buffer_to_texture(
                wgpu::TexelCopyBufferInfo {
                    buffer: src.buffer(),
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: u64::from(full) * u64::from(w) * 16,
                        bytes_per_row: None,
                        rows_per_image: None,
                    },
                },
                at(full),
                wgpu::Extent3d { width: rest, height: 1, depth_or_array_layers: 1 },
            );
        }
        gpu.submit([encoder.finish()]);
    });
    match error {
        Some(e) => Err(Error::new(format!("cannot copy the points into a picture: {e}"))),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests;
