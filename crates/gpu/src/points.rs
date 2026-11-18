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
//! Every buffer holds [`Points::capacity`] elements; the first [`Points::count`] are points. The
//! count is known on the CPU where a node made the points, or kept on the GPU where a node
//! dropped some ([`Count::Gpu`], [`compact`]): then the CPU knows only how many there may be.
//! A compute program runs over that many and returns for an index at or past the count. Nodes
//! that make points give them a capacity rounded up ([`capacity_for`]), so the buffers keep
//! their size while the count moves a little. A missing attribute reads as its default
//! ([`default_of`]): position 0, normal +z, colour white, scale 1.
//!
//! Layout in the buffers, as WGSL's `array<T>` has it: `f32`, `vec2f` (8 bytes), `vec3f` (16
//! bytes, the fourth float unused), `vec4f`, `u32`, `i32`.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use penumbra_core::{Error, ParamSpec, Value};
use penumbra_mesh::Bounds;

use crate::{Buffer, BufferRead, BufferRing, Dispatch, FLOAT_FORMAT, Gpu, Out, Step, Tex, TexRing};

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
    // Multiplies the width of a line drawn through the point.
    Standard { name: "width", ty: AttrType::F32, default: [1.0, 0.0, 0.0, 0.0] },
    // Along a trail, 0 at its head (the point now) to 1 at its tail (the oldest place).
    Standard { name: "trail", ty: AttrType::F32, default: [0.0; 4] },
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
    ("LineWidth", "width"),
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

/// The attribute that places the points, and that their bounds are of.
const POSITION: &str = "position";

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

/// Bytes of the header that holds a count on the GPU: four `u32`, the first the count, the
/// others 0.
pub const HEADER_BYTES: u64 = 16;

/// How many of the elements are points.
#[derive(Clone)]
#[non_exhaustive]
pub enum Count {
    /// Known on the CPU, as a generator knows it.
    Known(u32),
    /// Kept on the GPU, where a node that drops points wrote it: the first `u32` of `header`
    /// ([`HEADER_BYTES`]). Never more than `at_most`, which bounds it on the CPU and sizes the
    /// work over the points. `seen` is the count read back, a frame or more late, for display.
    Gpu { header: Buffer, at_most: u32, seen: Seen },
}

impl Count {
    /// The count on the GPU in `header`, at most `at_most`. Without room for any point it is
    /// known to be 0.
    pub fn gpu(header: Buffer, at_most: u32, seen: Seen) -> Result<Self, Error> {
        if header.bytes() < HEADER_BYTES {
            return Err(Error::new(format!(
                "the header of a count holds {HEADER_BYTES} bytes, not {}",
                header.bytes()
            )));
        }
        Ok(if at_most == 0 { Count::Known(0) } else { Count::Gpu { header, at_most, seen } })
    }

    /// The count where the CPU knows it.
    pub fn known(&self) -> Option<u32> {
        match self {
            Count::Known(n) => Some(*n),
            Count::Gpu { .. } => None,
        }
    }

    /// No more than this many: the count itself where known. Work over the points covers this
    /// many and skips those past the count.
    pub fn at_most(&self) -> u32 {
        match self {
            Count::Known(n) | Count::Gpu { at_most: n, .. } => *n,
        }
    }

    /// The count as last seen: the count itself where known, the last one read back from the
    /// GPU otherwise (`None` before the first arrives). For display and for channels, never
    /// for work on the GPU.
    pub fn seen(&self) -> Option<u32> {
        match self {
            Count::Known(n) => Some(*n),
            Count::Gpu { seen, at_most, .. } => seen.get().map(|n| n.min(*at_most)),
        }
    }

    /// The header on the GPU, where the count is kept there.
    pub fn header(&self) -> Option<&Buffer> {
        match self {
            Count::Known(_) => None,
            Count::Gpu { header, .. } => Some(header),
        }
    }

    /// The header a program reads the count from: this count's own, or for a count the CPU
    /// knows one that bounds nothing (`u32::MAX`), so the program's own bound applies. Shared by
    /// every program on `gpu`.
    pub fn header_for(&self, gpu: &Gpu) -> Result<Buffer, Error> {
        match self {
            Count::Gpu { header, .. } => Ok(header.clone()),
            Count::Known(_) => {
                let mut slot = crate::lock(&gpu.inner.no_limit);
                if let Some(b) = slot.as_ref() {
                    return Ok(b.clone());
                }
                let b = Buffer::new(gpu, HEADER_BYTES, "pts no limit")?;
                b.write(gpu, 0, &[u32::MAX; 4])?;
                *slot = Some(b.clone());
                Ok(b)
            }
        }
    }
}

impl PartialEq for Count {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Count::Known(a), Count::Known(b)) => a == b,
            (Count::Gpu { header: a, at_most: m, .. }, Count::Gpu { header: b, at_most: n, .. }) => {
                a.same(b) && m == n
            }
            _ => false,
        }
    }
}

impl std::fmt::Debug for Count {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Count::Known(n) => write!(f, "Known({n})"),
            Count::Gpu { at_most, seen, .. } => {
                write!(f, "Gpu {{ at_most: {at_most}, seen: {:?} }}", seen.get())
            }
        }
    }
}

/// A count on the GPU as read back: arrives a frame or more after it was written, without the
/// CPU waiting for it. Clones share it, so every node downstream sees the value once it
/// arrives. The engine polls the GPU every frame, which brings it in.
#[derive(Clone, Default)]
pub struct Seen(Arc<SeenInner>);

struct SeenInner {
    /// [`UNSEEN`] until a value arrived: this one's own, or until then the one it follows.
    value: AtomicU32,
    /// This one's own value, [`UNSEEN`] until its readback arrived.
    own: AtomicU32,
    read: Mutex<Option<BufferRead>>,
}

impl Default for SeenInner {
    fn default() -> Self {
        Self { value: AtomicU32::new(UNSEEN), own: AtomicU32::new(UNSEEN), read: Mutex::new(None) }
    }
}

impl SeenInner {
    /// Takes in the readback's value where it arrived; drops a readback that failed.
    fn settle(&self, read: &mut Option<BufferRead>) {
        let Some(r) = read.as_ref() else { return };
        match r.state() {
            crate::ReadState::Ready => {
                if let Some(n) = r.get::<u32>().ok().and_then(|w| w.first().copied()) {
                    let n = n.min(UNSEEN - 1);
                    self.own.store(n, Ordering::Relaxed);
                    self.value.store(n, Ordering::Relaxed);
                }
            }
            crate::ReadState::Failed => *read = None,
            crate::ReadState::Waiting => {}
        }
    }
}

const UNSEEN: u32 = u32::MAX;

impl Seen {
    /// Starts reading the count in `header` back. Until it arrives, [`Seen::get`] gives what
    /// `before` (the count this one follows, a frame earlier) had seen, so a display does not
    /// flicker. `before`'s finished readback is reused, so a readback per frame allocates
    /// nothing.
    pub fn reading(gpu: &Gpu, header: &Buffer, before: Option<&Seen>) -> Result<Self, Error> {
        let spare = before.and_then(|b| {
            let mut read = crate::lock(&b.0.read);
            // Its value first, so a readback that arrives just now is not lost with it.
            b.0.settle(&mut read);
            match read.as_ref().map(BufferRead::state) {
                Some(crate::ReadState::Ready) => read.take(),
                _ => None,
            }
        });
        let value = before.and_then(Seen::get).unwrap_or(UNSEEN);
        let read = header.read_async(gpu, spare)?;
        Ok(Self(Arc::new(SeenInner {
            value: AtomicU32::new(value),
            own: AtomicU32::new(UNSEEN),
            read: Mutex::new(Some(read)),
        })))
    }

    /// The count as last read back; `None` before anything arrived.
    pub fn get(&self) -> Option<u32> {
        self.0.settle(&mut crate::lock(&self.0.read));
        let v = self.0.value.load(Ordering::Relaxed);
        (v != UNSEEN).then_some(v)
    }

    /// The count in the header this one reads, at the time it was read: `None` until its own
    /// readback arrived (never the value of the one it follows).
    pub fn arrived(&self) -> Option<u32> {
        self.0.settle(&mut crate::lock(&self.0.read));
        let v = self.0.own.load(Ordering::Relaxed);
        (v != UNSEEN).then_some(v)
    }
}

/// The capacity a node gives points it makes when `count` of them are wanted: the count
/// rounded up to a step of an eighth of the power of two at or above it, the step at least
/// 1024. So a count that moves a little keeps its capacity and the buffers of a chain are
/// reused instead of made again. Above 8192 points that is less than a quarter more than the
/// count. Never below the count, and never above `most` (the largest number of elements a
/// buffer holds) unless the count itself is: a count above `most` is the caller's to refuse.
pub fn capacity_for(count: u32, most: u32) -> u32 {
    if count == 0 {
        return 0;
    }
    let step = (count.checked_next_power_of_two().unwrap_or(1 << 31) / 8).max(1024);
    let rounded = u64::from(count).div_ceil(u64::from(step)) * u64::from(step);
    u32::try_from(rounded).unwrap_or(u32::MAX).min(most).max(count)
}

/// The most elements of `ty` one buffer on `gpu` holds.
pub fn most_elements(gpu: &Gpu, ty: AttrType) -> u32 {
    u32::try_from(Buffer::max_bytes(gpu) / ty.stride()).unwrap_or(u32::MAX)
}

/// How points join into lines (line strips): which point follows which along a line.
/// `geo.from_pts` draws a segment from each point to the next one on its line.
///
/// A node that keeps the count and the order of the points keeps their strips; a node that drops
/// points or changes their order drops them ([`Points::with_count`] with a count on the GPU,
/// [`compact`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Strips {
    /// None given: each point stands alone. Drawn as lines, all the points make one line in
    /// their order.
    #[default]
    None,
    /// Every `len` points in a row are one line, in order: points 0 to `len - 1`, then `len` to
    /// `2 len - 1`, and so on. `closed`: each line's last point joins its first again (a ring).
    Regular { len: u32, closed: bool },
    /// The points are a grid `cols` wide, row by row (as `dims` [rows, cols, 1] lays them out):
    /// lines along its `rows`, along its `columns`, or both (a wireframe of the grid).
    Grid { cols: u32, rows: bool, columns: bool },
}

impl Strips {
    /// Open lines of `len` points each.
    pub fn regular(len: u32) -> Self {
        Strips::Regular { len, closed: false }
    }
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
        self.count.clone()
    }

    /// These points with `count`. A count on the GPU drops `dims` and `strips`: they describe
    /// an order that no longer holds. `Err` when it may be more than the capacity.
    pub fn with_count(self, count: Count) -> Result<Self, Error> {
        if count.at_most() > self.capacity {
            return Err(Error::new(format!(
                "{} points do not fit a capacity of {}",
                count.at_most(),
                self.capacity
            )));
        }
        let gpu = count.known().is_none();
        let dims = if gpu { None } else { self.dims };
        let strips = if gpu { Strips::None } else { self.strips };
        let out = Self { count, dims: None, strips, ..self };
        Ok(out.with_dims(dims))
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

    /// These points with `attr` added, or in place of the attribute of its name. New positions
    /// drop the bounds: set them again with [`Points::with_bounds`] where they are known. `Err`
    /// when its buffer holds fewer elements than the capacity, or the points hold [`MAX_ATTRS`]
    /// already.
    pub fn with_attr(&self, attr: Attr) -> Result<Self, Error> {
        let bounds = if attr.name() == POSITION { None } else { self.bounds };
        let mut attrs: Vec<Attr> = self.attrs.iter().filter(|a| a.name() != attr.name()).cloned().collect();
        attrs.push(attr);
        check_attrs(self.capacity, &attrs)?;
        Ok(Self { attrs: attrs.into(), bounds, ..self.clone() })
    }

    /// These points without the attribute `name`; without `position`, without bounds too.
    pub fn without(&self, name: &str) -> Self {
        let name = canonical(name);
        let bounds = if name == POSITION { None } else { self.bounds };
        let attrs: Vec<Attr> = self.attrs.iter().filter(|a| a.name() != name).cloned().collect();
        Self { attrs: attrs.into(), bounds, ..self.clone() }
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
    /// `(i % width, i / width)` ([`texel_size`] of the capacity, so the picture keeps its size
    /// while the count moves), for drawing and for nodes that read pictures. A scalar becomes
    /// grey (`(v, v, v, 1)`), a `vec2f` red and green with alpha 1, a `vec3f` rgb with whatever
    /// the fourth float holds as alpha. Texels past a count the CPU knows hold nothing in
    /// particular; past a count on the GPU they are transparent black. `None` where the points
    /// do not have the attribute or have no points. `textures` keeps the node's pictures across
    /// cooks, `scratch` the buffers a conversion needs.
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
        let (w, h) = texel_size(self.capacity.max(count));
        if self.count.header().is_some() {
            // The copy below would bring the dead points along: write every texel instead.
            return self.picture(gpu, attr.name(), (w, h), true, textures, scratch).map(Some);
        }
        let tex = textures.next_in(gpu, w, h, FLOAT_FORMAT, &format!("pts {}", attr.name()))?;
        let texels = match attr.ty() {
            AttrType::Vec3 | AttrType::Vec4 => attr.buffer().clone(),
            ty => {
                let out = scratch.next(gpu, u64::from(self.capacity.max(count)) * 16, "pts texels")?;
                expand(gpu, attr.buffer(), ty, count, (w, h), &out)?;
                out
            }
        };
        copy_rows(gpu, &texels, count, &tex)?;
        Ok(Some(tex))
    }

    /// The attribute `name` as a picture of `width` by `height` texels, point `i` at texel
    /// `(i % width, i / width)`, every texel written: past the count it is transparent black,
    /// also where the count is on the GPU. A scalar becomes `(v, v, v, 1)`, a `vec2f`
    /// `(x, y, 0, 1)`, a `vec3f` `(x, y, z, 1)`; a `vec4f` keeps its four numbers. So alpha is 1
    /// where a point is, unless a `vec4f` says otherwise. A standard attribute the points lack
    /// holds its default at every point.
    ///
    /// The picture is [`FLOAT_FORMAT`] where the device filters 32-bit floats
    /// ([`Gpu::float_textures`]); elsewhere it is [`crate::FORMAT`], rounded to half floats, so
    /// that every node can sample it. `Err` for an attribute the points lack and penumbra does
    /// not know, and for a picture too small for the points.
    pub fn attr_picture(
        &self,
        gpu: &Gpu,
        name: &str,
        size: (u32, u32),
        textures: &mut TexRing,
        scratch: &mut BufferRing,
    ) -> Result<Tex, Error> {
        self.picture(gpu, name, size, gpu.float_textures(), textures, scratch)
    }

    /// [`Points::attr_picture`] in 32-bit floats (`float`) or half floats.
    fn picture(
        &self,
        gpu: &Gpu,
        name: &str,
        (width, height): (u32, u32),
        float: bool,
        textures: &mut TexRing,
        scratch: &mut BufferRing,
    ) -> Result<Tex, Error> {
        let count = self.count.at_most();
        if u64::from(width) * u64::from(height) < u64::from(count) || width == 0 || height == 0 {
            return Err(Error::new(format!("{count} points do not fit a picture of {width}x{height}")));
        }
        let attr = self.attr(name);
        let standard = default_of(name);
        let (kind, fill) = match (attr.map(Attr::ty), standard) {
            (Some(AttrType::F32), _) => (0, [0.0; 4]),
            (Some(AttrType::Vec2), _) => (1, [0.0; 4]),
            (Some(AttrType::U32), _) => (2, [0.0; 4]),
            (Some(AttrType::I32), _) => (3, [0.0; 4]),
            (Some(AttrType::Vec3), _) => (4, [0.0; 4]),
            (Some(AttrType::Vec4), _) => (5, [0.0; 4]),
            (None, Some(s)) => (6, texel_of(s.ty, s.default)),
            (None, None) => {
                return Err(Error::new(format!("the points have no attribute `{}`", canonical(name))));
            }
        };
        let (format, texel_bytes, align) =
            if float { (FLOAT_FORMAT, 16u32, 16u32) } else { (crate::FORMAT, 8, 32) };
        let stride = width.next_multiple_of(align);
        let tex = textures.next_in(gpu, width, height, format, &format!("pts {}", canonical(name)))?;
        let out = scratch.next(
            gpu,
            u64::from(stride) * u64::from(height) * u64::from(texel_bytes),
            "pts picture",
        )?;
        let src = match attr {
            Some(a) if count > 0 => a.buffer().clone(),
            _ => Buffer::new(gpu, 16, "pts no attribute")?,
        };
        let header = self.count.header_for(gpu)?;
        let program = gpu.cached_compute("pts picture", PICTURE, &picture_params())?;
        let param = |n: &str| match n {
            "kind" => Some(Value::Int(kind)),
            "count" => Some(Value::Int(i64::from(count))),
            "stride" => Some(Value::Int(i64::from(stride))),
            "half" => Some(Value::Int(i64::from(!float))),
            "fill" => Some(Value::Vec4(fill.map(f64::from))),
            _ => None,
        };
        let input =
            Dispatch { volumes: &[], pictures: &[], data: None, seconds: 0.0, frame: 0, param: &param };
        program.run_with(
            gpu,
            &[&src, &out, &header],
            &[Step { entry: "picture", out: Out::grid(width, height), input: &input }],
        )?;
        copy_picture(gpu, &out, stride * texel_bytes, &tex)?;
        Ok(tex)
    }
}

/// The texel a value of type `ty` becomes in [`Points::attr_picture`].
fn texel_of(ty: AttrType, v: [f32; 4]) -> [f32; 4] {
    match ty {
        AttrType::F32 | AttrType::U32 | AttrType::I32 => [v[0], v[0], v[0], 1.0],
        AttrType::Vec2 => [v[0], v[1], 0.0, 1.0],
        AttrType::Vec3 => [v[0], v[1], v[2], 1.0],
        AttrType::Vec4 => v,
    }
}

/// Writes every texel of a picture from the elements of an attribute, into a buffer laid out
/// for a copy into a texture: rows of `params.stride` texels, 32-bit floats or half floats.
const PICTURE: &str = "
@group(2) @binding(0) var<storage, read> words: array<u32>;
@group(2) @binding(1) var<storage, read_write> texels: array<u32>;
// The count where it is on the GPU, else u32::MAX.
@group(2) @binding(2) var<uniform> pts_header: vec4u;

fn pts_word(i: u32) -> f32 {
    return bitcast<f32>(words[i]);
}

// A half float from a float, rounded to the nearest (ties to even). pack2x16float would do it,
// but needs a shader capability not every device has.
fn pts_half(f: f32) -> u32 {
    let x = bitcast<u32>(f);
    let sign = (x >> 16u) & 0x8000u;
    if (x & 0x7fffffffu) > 0x7f800000u {
        return sign | 0x7e00u;
    }
    let e = i32((x >> 23u) & 0xffu) - 112;
    var m = x & 0x7fffffu;
    if e >= 31 {
        return sign | 0x7c00u;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        m = m | 0x800000u;
        let shift = u32(14 - e);
        var r = m >> shift;
        let rest = m & ((1u << shift) - 1u);
        let mid = 1u << (shift - 1u);
        if rest > mid || (rest == mid && (r & 1u) == 1u) {
            r += 1u;
        }
        return sign | r;
    }
    var r = (u32(e) << 10u) | (m >> 13u);
    let rest = m & 0x1fffu;
    if rest > 0x1000u || (rest == 0x1000u && (r & 1u) == 1u) {
        r += 1u;
    }
    return sign | r;
}

fn pts_pack(a: f32, b: f32) -> u32 {
    return pts_half(a) | (pts_half(b) << 16u);
}

@compute @workgroup_size(64)
fn picture(@builtin(global_invocation_id) id: vec3u) {
    if id.x >= pn.size.x || id.y >= pn.size.y {
        return;
    }
    let i = id.y * pn.size.x + id.x;
    var v = vec4f(0.0);
    if i < min(u32(max(params.count, 0)), pts_header.x) {
        switch params.kind {
            case 0: {
                v = vec4f(vec3f(pts_word(i)), 1.0);
            }
            case 1: {
                v = vec4f(pts_word(2u * i), pts_word(2u * i + 1u), 0.0, 1.0);
            }
            case 2: {
                v = vec4f(vec3f(f32(words[i])), 1.0);
            }
            case 3: {
                v = vec4f(vec3f(f32(bitcast<i32>(words[i]))), 1.0);
            }
            case 4: {
                v = vec4f(pts_word(4u * i), pts_word(4u * i + 1u), pts_word(4u * i + 2u), 1.0);
            }
            case 5: {
                v = vec4f(pts_word(4u * i), pts_word(4u * i + 1u), pts_word(4u * i + 2u), pts_word(4u * i + 3u));
            }
            default: {
                v = params.fill;
            }
        }
    }
    let o = id.y * u32(params.stride) + id.x;
    if params.half == 1 {
        texels[2u * o] = pts_pack(v.x, v.y);
        texels[2u * o + 1u] = pts_pack(v.z, v.w);
    } else {
        texels[4u * o] = bitcast<u32>(v.x);
        texels[4u * o + 1u] = bitcast<u32>(v.y);
        texels[4u * o + 2u] = bitcast<u32>(v.z);
        texels[4u * o + 3u] = bitcast<u32>(v.w);
    }
}
";

fn picture_params() -> [ParamSpec; 5] {
    [
        ParamSpec::int("kind", 0),
        ParamSpec::int("count", 0),
        ParamSpec::int("stride", 0),
        ParamSpec::int("half", 0),
        ParamSpec::vec4("fill", [0.0; 4]),
    ]
}

/// Copies the whole of `tex` from `src`, rows `row_bytes` apart.
fn copy_picture(gpu: &Gpu, src: &Buffer, row_bytes: u32, tex: &Tex) -> Result<(), Error> {
    let (w, h) = tex.size();
    let ((), error) = gpu.scoped(|| {
        let mut encoder = gpu
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("pts picture") });
        encoder.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: src.buffer(),
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row_bytes),
                    rows_per_image: None,
                },
            },
            tex.texture().as_image_copy(),
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        gpu.submit([encoder.finish()]);
    });
    match error {
        Some(e) => Err(Error::new(format!("cannot copy the points into a picture: {e}"))),
        None => Ok(()),
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

pub mod append;
pub mod compact;
pub mod program;
pub mod reach;
pub mod trail;

#[cfg(test)]
mod tests;
