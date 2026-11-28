//! Volumes (3D textures) and compute programs that read and write them: what a simulation in
//! three dimensions and a ray-march need, beside the fragment passes of [`crate::pass`].
//!
//! A [`Volume`] is a grid of cells of [`VOLUME_FORMAT`] (RGBA, 16-bit float), filtered linearly
//! when sampled. A [`Compute`] program is WGSL with compute entry points; each writes one target,
//! a volume or a picture (a [`Tex`] made by [`Tex::new_storage`]), or only the program's own
//! buffers. The entry point's workgroup size says which: `@workgroup_size(4, 4, 4)`
//! writes a volume, `@workgroup_size(8, 8, 1)` a picture, any other size buffers only
//! ([`Target::Grid`]). A dispatch covers the target, or the cells [`Out::Size`] names, so a
//! shader returns early for ids outside `pn.size`.
//!
//! A program's own buffers ([`Buffer`]) are `@group(2)`, bindings 0, 1, 2 and on without gaps,
//! each `var<storage, read>` or `var<storage, read_write>` of any type a storage buffer holds (an
//! array of structs, `array<atomic<i32>>`), or `var<uniform>` for a struct of constants (faster
//! to read than the same struct in a storage buffer). [`Compute::encode_with`] binds one buffer to
//! each, the same ones for all of its steps. Values that change from step to step go in
//! `params`: every step has its own.
//!
//! The source gets a prelude appended, as a pass does:
//!
//! ```wgsl
//! var<uniform> pn: PnGrid;           // pn.size: the target's cells (x, y, z), z 1 for a picture
//! var<uniform> pn_time: PnTime;      // pn_time.seconds, pn_time.frame
//! var<uniform> params: Params;       // one field per declared numeric parameter
//! var vol0, vol1, vol2, vol3: texture_3d<f32>;  // volume inputs, transparent black when unbound
//! var pic0, pic1: texture_2d<f32>;              // picture inputs, the same
//! var pn_clamp: sampler;                        // linear filtering, clamped at the edges
//! var vol_out: texture_storage_3d<rgba16float, write>;   // the target, for a volume entry point
//! var pic_out: texture_storage_2d<rgba16float, write>;   // the target, for a picture entry point
//! var pn_data: array<vec4f>;                    // read-only numbers ([`DataBlock`]), one zero vec4f when unbound
//! // @group(2): the program's own buffers, declared in its source
//! fn pn_hash, pn_noise, pn_value_noise, pn_unpremultiply, pn_luma   // as in passes
//! ```
//!
//! Reading and writing one volume in the same dispatch is a usage conflict: simulations step
//! from one volume into another ([`VolumeRing`]). The conflict, like every other validation
//! error, comes back as an `Err` of the dispatch.
//!
//! State snapshots keep pictures only, so a volume goes into one as an atlas, its slices side by
//! side ([`Gpu::volume_to_atlas`], [`Gpu::volume_from_atlas`]).

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use penumbra_core::{Error, ParamSpec, Value, lock};

use crate::buffer::BufferKey;
use crate::pass::{self, Field, SHARED};
use crate::{Batch, Buffer, FORMAT, Gpu, Tex};

/// The format of every volume's cells.
pub const VOLUME_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Most cells of a volume: 64 MiB of half floats. A simulation keeps a few volumes of its grid.
pub const MAX_VOLUME_CELLS: u64 = 1 << 23;

/// Volume inputs a program has (`vol0` to `vol3`), and picture inputs (`pic0`, `pic1`).
pub const VOLUME_INPUTS: usize = 4;
pub const PICTURE_INPUTS: usize = 2;

/// The bind group of a program's own buffers.
pub const BUFFER_GROUP: u32 = 2;

/// Storage buffers the prelude binds itself (`pn_data`), counted against the device's limit of
/// storage buffers per shader stage.
pub const PRELUDE_STORAGE_BUFFERS: u32 = 1;

/// Uniforms the prelude binds itself (`pn`, `pn_time`, `params`).
const PRELUDE_UNIFORMS: u32 = 3;

const PRELUDE: &str = r#"
// ---- penumbra compute prelude ----
struct PnGrid { size: vec3u, _pn0: u32 }
struct PnTime { seconds: f32, frame: f32, _pn0: f32, _pn1: f32 }
@group(0) @binding(0) var<uniform> pn: PnGrid;
@group(0) @binding(1) var<uniform> pn_time: PnTime;
@group(0) @binding(2) var<uniform> params: Params;
@group(1) @binding(0) var vol0: texture_3d<f32>;
@group(1) @binding(1) var vol1: texture_3d<f32>;
@group(1) @binding(2) var vol2: texture_3d<f32>;
@group(1) @binding(3) var vol3: texture_3d<f32>;
@group(1) @binding(4) var pic0: texture_2d<f32>;
@group(1) @binding(5) var pic1: texture_2d<f32>;
@group(1) @binding(6) var pn_clamp: sampler;
@group(1) @binding(7) var vol_out: texture_storage_3d<rgba16float, write>;
@group(1) @binding(8) var pic_out: texture_storage_2d<rgba16float, write>;
@group(1) @binding(9) var<storage, read> pn_data: array<vec4f>;
"#;

struct VolumeInner {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    size: [u32; 3],
}

/// A 3D texture of [`VOLUME_FORMAT`]. Cheap to clone; clones share the texture.
#[derive(Clone)]
pub struct Volume(Arc<VolumeInner>);

impl Volume {
    /// A volume of `size` cells, zero everywhere. `Err` for a side of 0, one over the device's
    /// limit, or more than [`MAX_VOLUME_CELLS`] cells.
    pub fn new(gpu: &Gpu, size: [u32; 3], label: &str) -> Result<Self, Error> {
        check_size(gpu, size)?;
        let [width, height, depth] = size;
        let (texture, error) = gpu.scoped(|| {
            gpu.device().create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: depth },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D3,
                format: VOLUME_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        });
        if let Some(e) = error {
            return Err(Error::new(format!("cannot create a {width}x{height}x{depth} volume: {e}")));
        }
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Ok(Self(Arc::new(VolumeInner { texture, view, size })))
    }

    pub fn size(&self) -> [u32; 3] {
        self.0.size
    }

    pub fn cells(&self) -> u64 {
        self.0.size.iter().map(|s| u64::from(*s)).product()
    }

    /// Bytes on the GPU.
    pub fn bytes(&self) -> u64 {
        self.cells() * 8
    }

    pub fn texture(&self) -> &wgpu::Texture {
        &self.0.texture
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.0.view
    }

    pub fn same(&self, other: &Volume) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    fn key(&self) -> VolumeKey {
        VolumeKey(Arc::downgrade(&self.0))
    }

    fn shared(&self) -> bool {
        Arc::strong_count(&self.0) > 1
    }
}

/// A shape that made a volume what it is, for drawing it over a picture of the volume.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MediumShape {
    /// Its centre in the world, in metres.
    pub center: [f32; 3],
    /// Half its size along x, y and z, for a box.
    pub half: [f32; 3],
    /// Its radius, for a sphere.
    pub radius: f32,
    pub is_box: bool,
    /// An obstacle; an emitter when false.
    pub solid: bool,
}

/// A volume as it flows between nodes (`vol` data): the cells and the box they fill, in metres.
/// Channel use by convention: red the smoke's density, green its temperature, blue the fuel,
/// alpha free.
#[derive(Clone)]
pub struct Medium {
    pub volume: Volume,
    /// The box's centre and its size along x, y and z.
    pub center: [f32; 3],
    pub size: [f32; 3],
    /// What the simulation was given, for drawing it.
    pub shapes: Arc<[MediumShape]>,
}

/// Whether a volume of `size` can be made on `gpu`, with the reason when not.
pub fn check_size(gpu: &Gpu, size: [u32; 3]) -> Result<(), Error> {
    let max = gpu.device().limits().max_texture_dimension_3d;
    let cells: u64 = size.iter().map(|s| u64::from(*s)).product();
    if size.iter().any(|s| *s == 0 || *s > max) {
        let [x, y, z] = size;
        return Err(Error::new(format!("a volume of {x}x{y}x{z} cells must be 1 to {max} on each side")));
    }
    if cells > MAX_VOLUME_CELLS {
        let [x, y, z] = size;
        return Err(Error::new(format!(
            "a volume of {x}x{y}x{z} is {cells} cells, more than the {MAX_VOLUME_CELLS} allowed"
        )));
    }
    Ok(())
}

struct VolumeKey(Weak<VolumeInner>);

impl VolumeKey {
    fn is(&self, v: &Volume) -> bool {
        std::ptr::eq(self.0.as_ptr(), Arc::as_ptr(&v.0))
    }
}

/// Most `vec4f`s of a [`DataBlock`].
pub const MAX_DATA: usize = 1 << 16;

struct DataInner {
    buffer: wgpu::Buffer,
    capacity: usize,
}

/// Exact 32-bit numbers for a compute program, read in the shader as `pn_data`, an array of
/// `vec4f`. Half-float pictures round a position to about a thousandth of its size, which is
/// visible when something moves slowly against small cells. Cheap to clone; clones share the
/// buffer.
#[derive(Clone)]
pub struct DataBlock(Arc<DataInner>);

impl DataBlock {
    /// A block of `capacity` `vec4f`s (at least one), all zero. `Err` above [`MAX_DATA`].
    pub fn new(gpu: &Gpu, capacity: usize, label: &str) -> Result<Self, Error> {
        if capacity > MAX_DATA {
            return Err(Error::new(format!("a data block holds at most {MAX_DATA} vectors, not {capacity}")));
        }
        let capacity = capacity.max(1);
        let (buffer, error) = gpu.scoped(|| {
            gpu.device().create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: capacity as u64 * 16,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
        if let Some(e) = error {
            return Err(Error::new(format!("cannot create a data block of {capacity} vectors: {e}")));
        }
        Ok(Self(Arc::new(DataInner { buffer, capacity })))
    }

    /// Replaces the first `values.len()` vectors, for the dispatches recorded after this. `Err`
    /// when they do not fit.
    pub fn write(&self, gpu: &Gpu, values: &[[f32; 4]]) -> Result<(), Error> {
        if values.len() > self.0.capacity {
            return Err(Error::new(format!(
                "{} vectors do not fit a data block of {}",
                values.len(),
                self.0.capacity
            )));
        }
        let bytes: Vec<u8> = values.iter().flatten().flat_map(|v| v.to_le_bytes()).collect();
        gpu.queue().write_buffer(&self.0.buffer, 0, &bytes);
        Ok(())
    }

    /// Bytes on the GPU.
    pub fn bytes(&self) -> u64 {
        self.0.capacity as u64 * 16
    }

    fn key(&self) -> DataKey {
        DataKey(Arc::downgrade(&self.0))
    }
}

struct DataKey(Weak<DataInner>);

impl DataKey {
    fn is(&self, d: &DataBlock) -> bool {
        std::ptr::eq(self.0.as_ptr(), Arc::as_ptr(&d.0))
    }
}

/// Volumes of one size reused from step to step, as [`crate::TexRing`] does for pictures.
#[derive(Default)]
pub struct VolumeRing {
    volumes: Vec<Volume>,
}

const RING: usize = 3;

impl VolumeRing {
    /// A volume of `size` that nobody else holds: a kept one, or a new one (zero everywhere).
    /// A kept one holds what was written into it last, so a step writes every cell.
    pub fn next(&mut self, gpu: &Gpu, size: [u32; 3], label: &str) -> Result<Volume, Error> {
        self.volumes.retain(|v| v.size() == size);
        if let Some(v) = self.volumes.iter().find(|v| !v.shared()) {
            return Ok(v.clone());
        }
        let v = Volume::new(gpu, size, label)?;
        if self.volumes.len() >= RING {
            self.volumes.remove(0);
        }
        self.volumes.push(v.clone());
        Ok(v)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Volume> {
        self.volumes.iter()
    }

    pub fn clear(&mut self) {
        self.volumes.clear();
    }
}

/// The bytes of distinct volumes among `volumes`.
pub fn unique_volume_bytes<'a>(volumes: impl IntoIterator<Item = &'a Volume>) -> u64 {
    let mut seen: Vec<&Volume> = Vec::new();
    for v in volumes {
        if !seen.iter().any(|s| s.same(v)) {
            seen.push(v);
        }
    }
    seen.iter().map(|v| v.bytes()).sum()
}

/// What an entry point writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// Workgroup size (4, 4, 4): `vol_out`.
    Volume,
    /// Workgroup size (8, 8, 1): `pic_out`.
    Picture,
    /// Any other workgroup size: no picture or volume, only the program's own buffers.
    Grid,
}

impl Target {
    fn of(workgroup: [u32; 3]) -> Self {
        match workgroup {
            [4, 4, 4] => Target::Volume,
            [8, 8, 1] => Target::Picture,
            _ => Target::Grid,
        }
    }
}

/// The target of one dispatch.
#[derive(Clone, Copy)]
pub enum Out<'a> {
    Volume(&'a Volume),
    Picture(&'a Tex),
    /// No picture or volume: a dispatch over this many cells along x, y and z, for an entry point
    /// of any workgroup size. `pn.size` holds them, so the shader skips ids beyond them. A count
    /// of 0 dispatches nothing.
    Size([u32; 3]),
}

impl Out<'_> {
    /// [`Out::Size`] over `count` elements, for a list of particles.
    pub fn range(count: u32) -> Self {
        Out::Size([count, 1, 1])
    }

    /// [`Out::Size`] over a grid of `width` by `height` cells.
    pub fn grid(width: u32, height: u32) -> Self {
        Out::Size([width, height, 1])
    }

    fn target(&self) -> Option<Target> {
        match self {
            Out::Volume(_) => Some(Target::Volume),
            Out::Picture(_) => Some(Target::Picture),
            Out::Size(_) => None,
        }
    }

    fn size(&self) -> [u32; 3] {
        match self {
            Out::Volume(v) => v.size(),
            Out::Picture(t) => [t.width(), t.height(), 1],
            Out::Size(size) => *size,
        }
    }
}

/// The inputs and values of one dispatch.
pub struct Dispatch<'a> {
    pub volumes: &'a [Option<&'a Volume>],
    pub pictures: &'a [Option<&'a Tex>],
    pub data: Option<&'a DataBlock>,
    pub seconds: f64,
    pub frame: i64,
    pub param: &'a dyn Fn(&str) -> Option<Value>,
}

struct Entry {
    name: String,
    target: Target,
    workgroup: [u32; 3],
    pipeline: wgpu::ComputePipeline,
}

/// How a program declares one of its own buffers, at the binding of its index in `@group(2)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BufferSlot {
    /// `var<uniform>`: constants, read faster than from a storage buffer when every thread
    /// reads the same ones. Up to the device's `max_uniform_buffer_binding_size` (64 KiB by
    /// default).
    Uniform,
    /// `var<storage, read>`.
    Read,
    /// `var<storage, read_write>`, also for atomics.
    ReadWrite,
}

/// A compiled compute program: its entry points, sharing one set of parameters.
pub struct Compute {
    label: String,
    entries: Vec<Entry>,
    fields: Vec<Field>,
    uniform_size: u64,
    layouts: Arc<Layouts>,
    buffers: Vec<BufferSlot>,
    /// The layout of `@group(2)`, for a program that has buffers.
    buffer_layout: Option<wgpu::BindGroupLayout>,
    /// Per function and entry point: the globals it reads or writes, also through the
    /// functions it calls.
    uses: HashMap<String, Vec<String>>,
    /// Bind groups of `@group(2)` made lately, by the buffers they bind.
    groups: Arc<BufferGroups>,
}

/// The buffer bind groups of one [`Compute`]. [`Gpu::poll`] sweeps out the dead ones now and
/// then ([`Gpu::sweep_bind_groups`]), also of programs no node runs any more.
pub(crate) type BufferGroups = std::sync::Mutex<Vec<BufferGroup>>;

/// Buffer bind groups a [`Compute`] keeps. A node's buffers usually circulate through two or
/// three of a ring, and a built-in program is shared by every node of its kind.
const BUFFER_GROUPS: usize = 32;

/// The bind group of one list of buffers. A buffer never changes its size or memory once
/// made (a resize makes a new one), so the same buffers in the same order always bind the
/// same way. The keys do not hold the buffers.
pub(crate) struct BufferGroup {
    keys: Vec<BufferKey>,
    group: wgpu::BindGroup,
}

impl BufferGroup {
    fn matches(&self, buffers: &[&Buffer]) -> bool {
        self.keys.len() == buffers.len() && self.keys.iter().zip(buffers).all(|(k, b)| k.is(b))
    }

    /// Whether every buffer it binds is still held by someone besides the bind group.
    pub(crate) fn alive(&self) -> bool {
        self.keys.iter().all(BufferKey::alive)
    }
}

impl Compute {
    /// Compiles `source` with the prelude. `Err` with the compiler's message for WGSL that does
    /// not compile, for an entry point whose workgroup is larger than the device allows, and for
    /// bindings of the program's own that are no buffers in `@group(2)` at bindings 0 on without
    /// gaps, or more of them than the device binds.
    pub fn compile(gpu: &Gpu, label: &str, source: &str, params: &[ParamSpec]) -> Result<Self, Error> {
        let full = format!("{source}\n{PRELUDE}{SHARED}{}", pass::params_wgsl(params));
        let module = naga::front::wgsl::parse_str(&full)
            .map_err(|e| Error::new(pass::ascii_frame(&e.emit_to_string_with_path(&full, label))))?;
        let info = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), Default::default())
            .validate(&module)
            .map_err(|e| Error::new(pass::ascii_frame(&e.emit_to_string_with_path(&full, label))))?;
        let (fields, uniform_size) = pass::layout(&module, params)?;
        let buffers = buffer_slots(gpu, &module, &full, label)?;
        let uses = global_uses(&module, &info);
        let layouts = gpu.compute_layouts();
        let limits = gpu.device().limits();
        let most = [
            limits.max_compute_workgroup_size_x,
            limits.max_compute_workgroup_size_y,
            limits.max_compute_workgroup_size_z,
        ];
        let max_invocations = limits.max_compute_invocations_per_workgroup;
        let mut wanted = Vec::new();
        for e in module.entry_points.iter().filter(|e| e.stage == naga::ShaderStage::Compute) {
            let [x, y, z] = e.workgroup_size;
            let invocations = u64::from(x) * u64::from(y) * u64::from(z);
            if e.workgroup_size.iter().zip(most).any(|(s, m)| *s > m)
                || invocations > u64::from(max_invocations)
            {
                let [mx, my, mz] = most;
                return Err(Error::new(format!(
                    "{label}: entry point `{}` has workgroup size ({x}, {y}, {z}); this GPU takes at most \
                     ({mx}, {my}, {mz}) and {max_invocations} invocations in all",
                    e.name
                )));
            }
            wanted.push((e.name.clone(), Target::of(e.workgroup_size), e.workgroup_size));
        }
        if wanted.is_empty() {
            return Err(Error::new(format!("{label}: no compute entry point")));
        }
        let device = gpu.device();
        let ((entries, buffer_layout), error) = gpu.scoped(|| {
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(full.as_str().into()),
            });
            let own = (!buffers.is_empty()).then(|| layouts.with_buffers(gpu, &buffers));
            let pipeline_layout = own.as_ref().map_or(&layouts.pipeline, |(_, p)| p);
            let entries = wanted
                .into_iter()
                .map(|(name, target, workgroup)| {
                    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                        label: Some(&format!("{label} {name}")),
                        layout: Some(pipeline_layout),
                        module: &shader,
                        entry_point: Some(&name),
                        compilation_options: Default::default(),
                        cache: None,
                    });
                    Entry { name, target, workgroup, pipeline }
                })
                .collect::<Vec<_>>();
            (entries, own.map(|(group, _)| group))
        });
        if let Some(e) = error {
            return Err(Error::new(format!("{label}: {e}")));
        }
        Ok(Self {
            label: label.to_string(),
            entries,
            fields,
            uniform_size,
            layouts,
            buffers,
            buffer_layout,
            uses,
            groups: gpu.buffer_groups(),
        })
    }

    /// Whether function or entry point `function` reads or writes the global `global`, itself or
    /// through a function it calls. False for names the program does not have.
    pub fn uses(&self, function: &str, global: &str) -> bool {
        self.uses.get(function).is_some_and(|g| g.iter().any(|n| n == global))
    }

    /// The names of the entry points and what each writes.
    pub fn entries(&self) -> impl Iterator<Item = (&str, Target)> {
        self.entries.iter().map(|e| (e.name.as_str(), e.target))
    }

    /// The program's own buffers, by binding in `@group(2)`.
    pub fn buffers(&self) -> &[BufferSlot] {
        &self.buffers
    }

    /// Records entry point `entry` writing `out` as part of `batch` (submitted with it, or with
    /// the cook pass). `Err` for an entry point that does not exist or writes the other kind of
    /// target, a picture made without storage, and any validation error of the recording.
    pub fn encode(
        &self,
        batch: &mut Batch<'_>,
        entry: &str,
        out: Out<'_>,
        input: &Dispatch<'_>,
    ) -> Result<(), Error> {
        self.record(batch.gpu(), &[], &[Step { entry, out, input }])
    }

    /// Records `steps` in turn into one command encoder: as [`Compute::encode`] for each, at a
    /// fraction of the CPU's cost per step. A step reads what the steps before it wrote. `Err`
    /// as for one step; then none of them runs.
    pub fn encode_steps(&self, batch: &mut Batch<'_>, steps: &[Step<'_>]) -> Result<(), Error> {
        self.record(batch.gpu(), &[], steps)
    }

    /// [`Compute::encode_steps`] for a program with buffers of its own: `buffers` bind to
    /// `@group(2)` in binding order, for every step. All steps go into one compute pass, each
    /// with its own uniforms, so a simulation records the substeps of a frame in one call. `Err`
    /// when the number of buffers differs from the program's, for a dispatch of more workgroups
    /// along an axis than the device takes, and for any validation error of the recording (a
    /// buffer smaller than the shader's type, one bound twice where it is written).
    pub fn encode_with(
        &self,
        batch: &mut Batch<'_>,
        buffers: &[&Buffer],
        steps: &[Step<'_>],
    ) -> Result<(), Error> {
        self.record(batch.gpu(), buffers, steps)
    }

    /// Runs entry point `entry` writing `out`: submitted at once, unless the calling thread
    /// defers ([`Gpu::defer`]).
    pub fn run(&self, gpu: &Gpu, entry: &str, out: Out<'_>, input: &Dispatch<'_>) -> Result<(), Error> {
        self.record(gpu, &[], &[Step { entry, out, input }])
    }

    /// [`Compute::encode_with`], submitted at once unless the calling thread defers.
    pub fn run_with(&self, gpu: &Gpu, buffers: &[&Buffer], steps: &[Step<'_>]) -> Result<(), Error> {
        self.record(gpu, buffers, steps)
    }

    /// [`Compute::run_with`] for several sets of buffers: each run's steps with its own buffers,
    /// all in one compute pass of one command encoder, so many small runs cost the CPU about what
    /// one does. A run reads what the runs before it wrote. `Err` as for
    /// [`Compute::run_with`]; then none of them runs.
    pub fn run_many(&self, gpu: &Gpu, runs: &[(&[&Buffer], &[Step<'_>])]) -> Result<(), Error> {
        self.record_runs(gpu, runs)
    }

    fn record(&self, gpu: &Gpu, buffers: &[&Buffer], steps: &[Step<'_>]) -> Result<(), Error> {
        self.record_runs(gpu, &[(buffers, steps)])
    }

    fn record_runs(&self, gpu: &Gpu, runs: &[(&[&Buffer], &[Step<'_>])]) -> Result<(), Error> {
        struct Ready<'e> {
            entry: &'e Entry,
            groups: [u32; 3],
            parts: [Vec<u8>; 3],
            /// The run it belongs to.
            run: usize,
        }
        let groups2 =
            runs.iter().map(|(buffers, _)| self.buffer_group(gpu, buffers)).collect::<Result<Vec<_>, _>>()?;
        let max_groups = gpu.device().limits().max_compute_workgroups_per_dimension;
        let steps: Vec<(usize, &Step<'_>)> =
            runs.iter().enumerate().flat_map(|(k, (_, steps))| steps.iter().map(move |s| (k, s))).collect();
        let mut ready = Vec::with_capacity(steps.len());
        // Steps that share a `Dispatch` in a row share its uniforms, packed once.
        let mut packed: Option<(*const Dispatch<'_>, Vec<u8>, Vec<u8>)> = None;
        for &(run, step) in &steps {
            let (entry, out, input) = (step.entry, step.out, step.input);
            let e = self
                .entries
                .iter()
                .find(|e| e.name == entry)
                .ok_or_else(|| Error::new(format!("{}: no entry point `{entry}`", self.label)))?;
            if let Some(wanted) = out.target()
                && e.target != wanted
            {
                return Err(Error::new(format!(
                    "{}: entry point `{entry}` writes a {:?}, not a {:?}",
                    self.label, e.target, wanted
                )));
            }
            if let Out::Picture(t) = out
                && (!t.is_storage() || t.format() != FORMAT)
            {
                return Err(Error::new(format!(
                    "{}: the target picture was not made for compute",
                    self.label
                )));
            }
            let size = out.size();
            let mut grid = vec![0u8; 16];
            for (slot, x) in grid.as_chunks_mut::<4>().0.iter_mut().zip(size) {
                slot.copy_from_slice(&x.to_le_bytes());
            }
            let key: *const Dispatch<'_> = input;
            let (time, params) = match &packed {
                Some((k, time, params)) if std::ptr::eq(*k, key) => (time.clone(), params.clone()),
                _ => {
                    let mut time = vec![0u8; 16];
                    for (slot, x) in
                        time.as_chunks_mut::<4>().0.iter_mut().zip([input.seconds as f32, input.frame as f32])
                    {
                        slot.copy_from_slice(&x.to_le_bytes());
                    }
                    let params = pass::pack(&self.fields, self.uniform_size, input.param);
                    packed = Some((key, time.clone(), params.clone()));
                    (time, params)
                }
            };
            let groups: [u32; 3] = std::array::from_fn(|i| {
                size.get(i).copied().unwrap_or(1).div_ceil(e.workgroup.get(i).copied().unwrap_or(1).max(1))
            });
            if let Some((axis, n)) = groups.iter().enumerate().find(|(_, n)| **n > max_groups) {
                let [x, y, z] = size;
                let wg = e.workgroup.get(axis).copied().unwrap_or(1);
                let along = ["x", "y", "z"].get(axis).copied().unwrap_or("an axis");
                return Err(Error::new(format!(
                    "{}: entry point `{entry}` over {x}x{y}x{z} cells needs {n} workgroups of {wg} along \
                     {along}, more than the {max_groups} this GPU dispatches",
                    self.label
                )));
            }
            ready.push(Ready { entry: e, groups, parts: [grid, time, params], run });
        }
        let parts: Vec<[&[u8]; 3]> = ready
            .iter()
            .map(|r| [r.parts[0].as_slice(), r.parts[1].as_slice(), r.parts[2].as_slice()])
            .collect();
        let error = gpu.record_many(&self.label, &parts, |encoder, slots| {
            let mut cp = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some(&self.label),
                timestamp_writes: None,
            });
            let mut bound = None;
            for ((r, (_, step)), slots) in ready.iter().zip(&steps).zip(slots) {
                let [x, y, z] = r.groups;
                if x == 0 || y == 0 || z == 0 {
                    continue;
                }
                if bound != Some(r.run) {
                    if let Some(Some(g)) = groups2.get(r.run) {
                        cp.set_bind_group(BUFFER_GROUP, g, &[]);
                    }
                    bound = Some(r.run);
                }
                let group1 = self.layouts.group(gpu, step.input, step.out);
                cp.set_pipeline(&r.entry.pipeline);
                cp.set_bind_group(0, &slots.group, &slots.offsets);
                cp.set_bind_group(1, &group1, &[]);
                cp.dispatch_workgroups(x, y, z);
            }
        });
        match error {
            Some(e) => {
                let names: Vec<&str> = steps.iter().map(|(_, s)| s.entry).collect();
                Err(Error::new(format!("{} {}: {e}", self.label, names.join(", "))))
            }
            None => Ok(()),
        }
    }

    /// The bind group of `buffers` over the program's `@group(2)`, made or kept; `None` for a
    /// program without buffers. Groups whose buffers are gone otherwise are let go, so the
    /// cache keeps no buffer's memory alive for long.
    fn buffer_group(&self, gpu: &Gpu, buffers: &[&Buffer]) -> Result<Option<wgpu::BindGroup>, Error> {
        if buffers.len() != self.buffers.len() {
            return Err(Error::new(format!(
                "{}: the program has {} buffers in @group({BUFFER_GROUP}), {} were given",
                self.label,
                self.buffers.len(),
                buffers.len()
            )));
        }
        let Some(layout) = &self.buffer_layout else { return Ok(None) };
        let most = gpu.device().limits().max_uniform_buffer_binding_size;
        for (binding, (slot, b)) in self.buffers.iter().zip(buffers).enumerate() {
            if *slot == BufferSlot::Uniform && b.bytes() > most {
                return Err(Error::new(format!(
                    "{}: the buffer at binding {binding} is a uniform of {} bytes; this GPU binds at most {most}",
                    self.label,
                    b.bytes()
                )));
            }
        }
        let mut kept = lock(&self.groups);
        if let Some(i) = kept.iter().position(|g| g.matches(buffers)) {
            let g = kept.remove(i);
            let group = g.group.clone();
            kept.push(g);
            return Ok(Some(group));
        }
        // Bound with their size, not whole: a backend may pad a buffer that can be a uniform,
        // and `arrayLength` would count the padding.
        let entries: Vec<wgpu::BindGroupEntry<'_>> = buffers
            .iter()
            .zip(0u32..)
            .map(|(b, binding)| wgpu::BindGroupEntry {
                binding,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: b.buffer(),
                    offset: 0,
                    size: wgpu::BufferSize::new(b.bytes()),
                }),
            })
            .collect();
        let (group, error) = gpu.scoped(|| {
            gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(&self.label),
                layout,
                entries: &entries,
            })
        });
        if let Some(e) = error {
            return Err(Error::new(format!("{}: cannot bind the buffers: {e}", self.label)));
        }
        kept.retain(BufferGroup::alive);
        if kept.len() >= BUFFER_GROUPS {
            kept.remove(0);
        }
        kept.push(BufferGroup { keys: buffers.iter().map(|b| b.key()).collect(), group: group.clone() });
        Ok(Some(group))
    }
}

/// The globals each function and entry point of `module` uses, by name.
fn global_uses(module: &naga::Module, info: &naga::valid::ModuleInfo) -> HashMap<String, Vec<String>> {
    let names = |f: &naga::valid::FunctionInfo| -> Vec<String> {
        module
            .global_variables
            .iter()
            .filter(|(h, _)| !f[*h].is_empty())
            .filter_map(|(_, g)| g.name.clone())
            .collect()
    };
    let mut out = HashMap::new();
    for (h, f) in module.functions.iter() {
        if let Some(name) = &f.name {
            out.insert(name.clone(), names(&info[h]));
        }
    }
    for (i, e) in module.entry_points.iter().enumerate() {
        out.insert(e.name.clone(), names(info.get_entry_point(i)));
    }
    out
}

/// The program's own buffers, from the bindings `module` declares in `@group(2)`. `Err`, naming
/// the line in `source`, for a binding there that is no buffer, for bindings in groups beyond
/// it, for gaps, and for more buffers of a kind than the device binds.
fn buffer_slots(
    gpu: &Gpu,
    module: &naga::Module,
    source: &str,
    label: &str,
) -> Result<Vec<BufferSlot>, Error> {
    let mut found: Vec<(u32, BufferSlot)> = Vec::new();
    for (handle, g) in module.global_variables.iter() {
        let Some(binding) = &g.binding else { continue };
        if binding.group < BUFFER_GROUP {
            continue;
        }
        let name = g.name.as_deref().unwrap_or("?");
        let line = module.global_variables.get_span(handle).location(source).line_number;
        let at = format!("{label}:{line}: `{name}`");
        if binding.group > BUFFER_GROUP {
            return Err(Error::new(format!(
                "{at} is in @group({}); a program's own buffers go in @group({BUFFER_GROUP})",
                binding.group
            )));
        }
        let slot = match g.space {
            naga::AddressSpace::Uniform => BufferSlot::Uniform,
            naga::AddressSpace::Storage { access } if access.contains(naga::StorageAccess::STORE) => {
                BufferSlot::ReadWrite
            }
            naga::AddressSpace::Storage { .. } => BufferSlot::Read,
            space => {
                let what = match space {
                    naga::AddressSpace::Handle => "a texture or sampler",
                    _ => "no buffer",
                };
                return Err(Error::new(format!(
                    "{at} is {what}; @group({BUFFER_GROUP}) holds buffers only, declared var<uniform>, \
                     var<storage, read> or var<storage, read_write>"
                )));
            }
        };
        found.push((binding.binding, slot));
    }
    found.sort_by_key(|(b, _)| *b);
    for (expected, (binding, _)) in (0u32..).zip(&found) {
        if *binding != expected {
            return Err(Error::new(format!(
                "{label}: the buffers in @group({BUFFER_GROUP}) take bindings 0, 1, 2 and on, without gaps \
                 or repeats; binding {expected} is missing or taken twice"
            )));
        }
    }
    let limits = gpu.device().limits();
    for (kind, most, prelude) in [
        ("storage buffers", limits.max_storage_buffers_per_shader_stage, PRELUDE_STORAGE_BUFFERS),
        ("uniforms", limits.max_uniform_buffers_per_shader_stage, PRELUDE_UNIFORMS),
    ] {
        let uniform = kind == "uniforms";
        let n = found.iter().filter(|(_, s)| (*s == BufferSlot::Uniform) == uniform).count();
        let allowed = most.saturating_sub(prelude);
        if n as u64 > u64::from(allowed) {
            return Err(Error::new(format!(
                "{label}: {n} {kind} in @group({BUFFER_GROUP}); this GPU binds {most} per shader stage and the \
                 prelude takes {prelude}, so a program has at most {allowed}"
            )));
        }
    }
    Ok(found.into_iter().map(|(_, s)| s).collect())
}

/// One dispatch of [`Compute::encode_steps`].
#[derive(Clone, Copy)]
pub struct Step<'a> {
    pub entry: &'a str,
    pub out: Out<'a>,
    pub input: &'a Dispatch<'a>,
}

/// The layouts every compute program shares, and the stand-ins for unbound slots.
pub(crate) struct Layouts {
    group1: wgpu::BindGroupLayout,
    pipeline: wgpu::PipelineLayout,
    sampler: wgpu::Sampler,
    empty_volume: wgpu::TextureView,
    empty_picture: wgpu::TextureView,
    spare_volume: wgpu::TextureView,
    spare_picture: wgpu::TextureView,
    empty_data: wgpu::Buffer,
    /// The last bind groups made, with what they bind, for reuse while all of it lives.
    groups: std::sync::Mutex<Vec<Group>>,
}

struct Group {
    volumes: [Option<VolumeKey>; VOLUME_INPUTS],
    pictures: [Option<crate::tex::TexKey>; PICTURE_INPUTS],
    data: Option<DataKey>,
    out: OutKey,
    group: wgpu::BindGroup,
}

enum OutKey {
    Volume(VolumeKey),
    Picture(crate::tex::TexKey),
    Nothing,
}

const GROUPS: usize = 48;

impl Group {
    fn matches(&self, input: &Dispatch<'_>, out: Out<'_>) -> bool {
        let volumes = self.volumes.iter().enumerate().all(|(i, k)| {
            match (k, input.volumes.get(i).copied().flatten()) {
                (None, None) => true,
                (Some(k), Some(v)) => k.is(v),
                _ => false,
            }
        });
        let pictures = self.pictures.iter().enumerate().all(|(i, k)| {
            match (k, input.pictures.get(i).copied().flatten()) {
                (None, None) => true,
                (Some(k), Some(t)) => k.is(t),
                _ => false,
            }
        });
        let data = match (&self.data, input.data) {
            (None, None) => true,
            (Some(k), Some(d)) => k.is(d),
            _ => false,
        };
        let out = match (&self.out, out) {
            (OutKey::Volume(k), Out::Volume(v)) => k.is(v),
            (OutKey::Picture(k), Out::Picture(t)) => k.is(t),
            (OutKey::Nothing, Out::Size(_)) => true,
            _ => false,
        };
        volumes && pictures && data && out
    }

    fn alive(&self) -> bool {
        let out = match &self.out {
            OutKey::Volume(k) => k.0.strong_count() > 0,
            OutKey::Picture(k) => k.alive(),
            OutKey::Nothing => true,
        };
        out && self.volumes.iter().flatten().all(|k| k.0.strong_count() > 0)
            && self.data.iter().all(|k| k.0.strong_count() > 0)
            && self.pictures.iter().flatten().all(crate::tex::TexKey::alive)
    }
}

impl Layouts {
    pub(crate) fn new(device: &wgpu::Device, group0: &wgpu::BindGroupLayout) -> Self {
        let sampled = |binding, dim| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: dim,
                multisampled: false,
            },
            count: None,
        };
        let storage = |binding, dim| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::StorageTexture {
                access: wgpu::StorageTextureAccess::WriteOnly,
                format: VOLUME_FORMAT,
                view_dimension: dim,
            },
            count: None,
        };
        let d3 = wgpu::TextureViewDimension::D3;
        let d2 = wgpu::TextureViewDimension::D2;
        let entries = [
            sampled(0, d3),
            sampled(1, d3),
            sampled(2, d3),
            sampled(3, d3),
            sampled(4, d2),
            sampled(5, d2),
            wgpu::BindGroupLayoutEntry {
                binding: 6,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            storage(7, d3),
            storage(8, d2),
            wgpu::BindGroupLayoutEntry {
                binding: 9,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ];
        let group1 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("compute inputs"),
            entries: &entries,
        });
        let pipeline = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("compute"),
            bind_group_layouts: &[Some(group0), Some(&group1)],
            immediate_size: 0,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("compute clamp"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let one = |label, dimension, format, usage| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension,
                    format,
                    usage,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        let sample = wgpu::TextureUsages::TEXTURE_BINDING;
        let write = wgpu::TextureUsages::STORAGE_BINDING;
        let (t3, t2) = (wgpu::TextureDimension::D3, wgpu::TextureDimension::D2);
        Self {
            group1,
            pipeline,
            sampler,
            empty_volume: one("unbound volume", t3, VOLUME_FORMAT, sample),
            empty_picture: one("unbound picture", t2, FORMAT, sample),
            spare_volume: one("unused volume target", t3, VOLUME_FORMAT, write),
            spare_picture: one("unused picture target", t2, FORMAT, write),
            empty_data: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("unbound data"),
                size: 16,
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            }),
            groups: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// The layout of `@group(2)` for `buffers`, and the pipeline layout of a program with them.
    fn with_buffers(
        &self,
        gpu: &Gpu,
        buffers: &[BufferSlot],
    ) -> (wgpu::BindGroupLayout, wgpu::PipelineLayout) {
        let device = gpu.device();
        let entries: Vec<wgpu::BindGroupLayoutEntry> = buffers
            .iter()
            .zip(0u32..)
            .map(|(slot, binding)| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: match slot {
                        BufferSlot::Uniform => wgpu::BufferBindingType::Uniform,
                        BufferSlot::Read => wgpu::BufferBindingType::Storage { read_only: true },
                        BufferSlot::ReadWrite => wgpu::BufferBindingType::Storage { read_only: false },
                    },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            })
            .collect();
        let group2 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("compute buffers"),
            entries: &entries,
        });
        let pipeline = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("compute with buffers"),
            bind_group_layouts: &[Some(&gpu.layouts().group0), Some(&self.group1), Some(&group2)],
            immediate_size: 0,
        });
        (group2, pipeline)
    }

    fn group(&self, gpu: &Gpu, input: &Dispatch<'_>, out: Out<'_>) -> wgpu::BindGroup {
        let mut kept = lock(&self.groups);
        if let Some(i) = kept.iter().position(|g| g.matches(input, out)) {
            let g = kept.remove(i);
            let group = g.group.clone();
            kept.push(g);
            return group;
        }
        kept.retain(Group::alive);
        if kept.len() >= GROUPS {
            kept.remove(0);
        }
        let volume =
            |i: usize| input.volumes.get(i).copied().flatten().map_or(&self.empty_volume, Volume::view);
        let picture =
            |i: usize| input.pictures.get(i).copied().flatten().map_or(&self.empty_picture, Tex::view);
        let (vol_out, pic_out) = match out {
            Out::Volume(v) => (v.view(), &self.spare_picture),
            Out::Picture(t) => (&self.spare_volume, t.view()),
            Out::Size(_) => (&self.spare_volume, &self.spare_picture),
        };
        let view = |binding, view| wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::TextureView(view),
        };
        let group = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("compute inputs"),
            layout: &self.group1,
            entries: &[
                view(0, volume(0)),
                view(1, volume(1)),
                view(2, volume(2)),
                view(3, volume(3)),
                view(4, picture(0)),
                view(5, picture(1)),
                wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                view(7, vol_out),
                view(8, pic_out),
                wgpu::BindGroupEntry {
                    binding: 9,
                    resource: input.data.map_or(&self.empty_data, |d| &d.0.buffer).as_entire_binding(),
                },
            ],
        });
        kept.push(Group {
            volumes: std::array::from_fn(|i| input.volumes.get(i).copied().flatten().map(Volume::key)),
            pictures: std::array::from_fn(|i| input.pictures.get(i).copied().flatten().map(Tex::key)),
            data: input.data.map(DataBlock::key),
            out: match out {
                Out::Volume(v) => OutKey::Volume(v.key()),
                Out::Picture(t) => OutKey::Picture(t.key()),
                Out::Size(_) => OutKey::Nothing,
            },
            group: group.clone(),
        });
        group
    }
}

/// Compiled programs by source and params, for [`Gpu::cached_compute`].
pub(crate) type Cache = HashMap<u64, Arc<Compute>>;

const ATLAS: &str = r#"
// A volume's slices side by side, `cols` to a row, slice 0 at the top left.
@compute @workgroup_size(8, 8, 1)
fn pack(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= pn.size.xy)) {
        return;
    }
    let s = textureDimensions(vol0);
    let tile = id.xy / s.xy;
    let z = tile.y * u32(params.cols) + tile.x;
    var c = vec4f(0.0);
    if (tile.x < u32(params.cols) && z < s.z) {
        c = textureLoad(vol0, vec3u(id.xy % s.xy, z), 0);
    }
    textureStore(pic_out, id.xy, c);
}

@compute @workgroup_size(4, 4, 4)
fn unpack(@builtin(global_invocation_id) id: vec3u) {
    if (any(id >= pn.size)) {
        return;
    }
    let cols = u32(max(params.cols, 1));
    let at = vec2u((id.z % cols) * pn.size.x + id.x, (id.z / cols) * pn.size.y + id.y);
    let v = textureLoad(pic0, at, 0);
    // A snapshot is read back from disk: nothing that is not a finite half float.
    let finite = select(v, vec4f(0.0), v != v);
    textureStore(vol_out, id, clamp(finite, vec4f(-65504.0), vec4f(65504.0)));
}
"#;

/// Columns of slices in the atlas of a volume of `size`: about as wide as high.
pub fn atlas_columns(size: [u32; 3]) -> u32 {
    let [x, y, z] = size.map(|s| f64::from(s.max(1)));
    ((z * y / x).sqrt().round() as u32).clamp(1, size[2].max(1))
}

/// The atlas's size in pixels for a volume of `size`.
pub fn atlas_size(size: [u32; 3]) -> (u32, u32) {
    let cols = atlas_columns(size);
    let rows = size[2].max(1).div_ceil(cols);
    (size[0].saturating_mul(cols), size[1].saturating_mul(rows))
}

fn atlas_params() -> Vec<ParamSpec> {
    vec![ParamSpec::int("cols", 1)]
}

impl Gpu {
    pub(crate) fn compute_layouts(&self) -> Arc<Layouts> {
        let mut slot = lock(&self.inner.compute);
        if let Some(l) = slot.as_ref() {
            return l.clone();
        }
        let l = Arc::new(Layouts::new(self.device(), &self.layouts().group0));
        *slot = Some(l.clone());
        l
    }

    /// A compute program compiled once per process and shared, as [`Gpu::cached_pass`] does.
    pub fn cached_compute(
        &self,
        label: &str,
        source: &str,
        params: &[ParamSpec],
    ) -> Result<Arc<Compute>, Error> {
        let key = pass::cache_key(source, params);
        if let Some(c) = lock(&self.inner.computes).get(&key) {
            return Ok(c.clone());
        }
        let compiled = Arc::new(Compute::compile(self, label, source, params)?);
        lock(&self.inner.computes).insert(key, compiled.clone());
        Ok(compiled)
    }

    /// The program [`Gpu::cached_compute`] compiled for `source` and `params` before, without
    /// compiling: `None` when nobody has.
    pub fn compiled_compute(&self, source: &str, params: &[ParamSpec]) -> Option<Arc<Compute>> {
        lock(&self.inner.computes).get(&pass::cache_key(source, params)).cloned()
    }

    /// `volume` as a picture of its slices side by side ([`atlas_size`]), for a state snapshot.
    pub fn volume_to_atlas(&self, volume: &Volume) -> Result<Tex, Error> {
        let (w, h) = atlas_size(volume.size());
        let atlas = Tex::new_storage(self, w, h, "volume atlas")?;
        self.pack_atlas(volume, &atlas)?;
        Ok(atlas)
    }

    /// Writes `volume`'s atlas into `atlas`, a storage picture of [`atlas_size`]: as
    /// [`Gpu::volume_to_atlas`] into a picture kept for it. Recorded like any other dispatch.
    pub fn pack_atlas(&self, volume: &Volume, atlas: &Tex) -> Result<(), Error> {
        if atlas.size() != atlas_size(volume.size()) {
            return Err(Error::new("the picture is not the size of the volume's atlas"));
        }
        let program = self.cached_compute("volume atlas", ATLAS, &atlas_params())?;
        let cols = i64::from(atlas_columns(volume.size()));
        let param = |n: &str| (n == "cols").then_some(Value::Int(cols));
        let input = Dispatch {
            volumes: &[Some(volume)],
            pictures: &[],
            data: None,
            seconds: 0.0,
            frame: 0,
            param: &param,
        };
        program.run(self, "pack", Out::Picture(atlas), &input)
    }

    /// A volume of `size` from an atlas [`Gpu::volume_to_atlas`] made. `Err` when the atlas has
    /// another size than one of `size` has.
    pub fn volume_from_atlas(&self, atlas: &Tex, size: [u32; 3]) -> Result<Volume, Error> {
        check_size(self, size)?;
        if atlas.size() != atlas_size(size) {
            let [x, y, z] = size;
            let (w, h) = atlas.size();
            return Err(Error::new(format!("a {w}x{h} picture is no atlas of a {x}x{y}x{z} volume")));
        }
        let volume = Volume::new(self, size, "volume from atlas")?;
        let program = self.cached_compute("volume atlas", ATLAS, &atlas_params())?;
        let cols = i64::from(atlas_columns(size));
        let param = |n: &str| (n == "cols").then_some(Value::Int(cols));
        let input = Dispatch {
            volumes: &[],
            pictures: &[Some(atlas)],
            data: None,
            seconds: 0.0,
            frame: 0,
            param: &param,
        };
        program.run(self, "unpack", Out::Volume(&volume), &input)?;
        Ok(volume)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::gpu;

    /// Writes a pattern into a volume, then reads it through a second volume and a picture.
    const PATTERN: &str = "
@compute @workgroup_size(4, 4, 4)
fn fill(@builtin(global_invocation_id) id: vec3u) {
    if (any(id >= pn.size)) {
        return;
    }
    textureStore(vol_out, id, vec4f(f32(id.x), f32(id.y), f32(id.z), params.gain));
}

@compute @workgroup_size(4, 4, 4)
fn double(@builtin(global_invocation_id) id: vec3u) {
    if (any(id >= pn.size)) {
        return;
    }
    textureStore(vol_out, id, textureLoad(vol0, id, 0) * 2.0);
}

// Linear filtering of a volume, sampled between cells, along z for the picture's x.
@compute @workgroup_size(8, 8, 1)
fn slice(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= pn.size.xy)) {
        return;
    }
    let s = vec3f(textureDimensions(vol0));
    let uvw = vec3f(0.5, f32(id.y) + 0.5, f32(id.x) * 0.5 + 0.5) / s;
    textureStore(pic_out, id.xy, textureSampleLevel(vol0, pn_clamp, uvw, 0.0));
}
";

    fn params() -> Vec<ParamSpec> {
        vec![ParamSpec::float("gain", 1.0)]
    }

    #[test]
    fn warp_writes_and_filters_volumes() {
        let Some(gpu) = gpu() else { return };
        let program = Compute::compile(&gpu, "pattern", PATTERN, &params()).unwrap();
        let names: Vec<_> = program.entries().collect();
        assert!(names.contains(&("fill", Target::Volume)) && names.contains(&("slice", Target::Picture)));
        let size = [5, 3, 7];
        let a = Volume::new(&gpu, size, "a").unwrap();
        let b = Volume::new(&gpu, size, "b").unwrap();
        let param = |n: &str| (n == "gain").then_some(Value::Float(0.25));
        let none =
            Dispatch { volumes: &[], pictures: &[], data: None, seconds: 0.0, frame: 0, param: &param };
        program.run(&gpu, "fill", Out::Volume(&a), &none).unwrap();
        let from_a = Dispatch {
            volumes: &[Some(&a)],
            pictures: &[],
            data: None,
            seconds: 0.0,
            frame: 0,
            param: &param,
        };
        program.run(&gpu, "double", Out::Volume(&b), &from_a).unwrap();

        let atlas = gpu.volume_to_atlas(&b).unwrap();
        assert_eq!(atlas.size(), atlas_size(size));
        let pixels = gpu.read(&atlas).unwrap();
        let cols = atlas_columns(size);
        for (x, y, z) in [(0, 0, 0), (4, 2, 6), (2, 1, 3)] {
            let at = ((z % cols) * size[0] + x, (z / cols) * size[1] + y);
            let got = pixels.get(at.0, at.1).unwrap();
            assert_eq!(got, [2.0 * x as f32, 2.0 * y as f32, 2.0 * z as f32, 0.5], "cell {x},{y},{z}");
        }

        // Back from the atlas: the same cells.
        let back = gpu.volume_from_atlas(&atlas, size).unwrap();
        let again = gpu.read(&gpu.volume_to_atlas(&back).unwrap()).unwrap();
        assert_eq!(again.get(9, 4), pixels.get(9, 4));

        let pic = Tex::new_storage(&gpu, 13, 3, "slice").unwrap();
        program.run(&gpu, "slice", Out::Picture(&pic), &from_a).unwrap();
        let p = gpu.read(&pic).unwrap();
        // Half a cell along z between cells 1 and 2: their mean.
        let z = p.get(3, 1).unwrap()[2];
        assert!((z - 1.5).abs() < 1e-3, "{z}");
    }

    #[test]
    fn a_dispatch_that_cannot_work_is_an_error_not_a_crash() {
        let Some(gpu) = gpu() else { return };
        let program = Compute::compile(&gpu, "pattern", PATTERN, &params()).unwrap();
        let v = Volume::new(&gpu, [4, 4, 4], "v").unwrap();
        let param = |_: &str| None;
        let reads_v = Dispatch {
            volumes: &[Some(&v)],
            pictures: &[],
            data: None,
            seconds: 0.0,
            frame: 0,
            param: &param,
        };
        // Reading and writing one volume at once.
        assert!(program.run(&gpu, "double", Out::Volume(&v), &reads_v).is_err());
        // An entry point that writes the other kind of target, or none at all.
        let plain = Tex::new(&gpu, 4, 4, "plain").unwrap();
        assert!(program.run(&gpu, "slice", Out::Volume(&v), &reads_v).is_err());
        assert!(program.run(&gpu, "slice", Out::Picture(&plain), &reads_v).is_err());
        assert!(program.run(&gpu, "missing", Out::Volume(&v), &reads_v).is_err());
        // Bad sizes and bad workgroups.
        assert!(Volume::new(&gpu, [0, 4, 4], "zero").is_err());
        assert!(Volume::new(&gpu, [1024, 1024, 16], "huge").is_err());
        let huge = "@compute @workgroup_size(32, 32, 1) fn f(@builtin(global_invocation_id) id: vec3u) {}";
        let e = Compute::compile(&gpu, "huge", huge, &[]).err().unwrap();
        assert!(e.0.contains("(32, 32, 1)") && e.0.contains("invocations"), "{e}");
        // Another workgroup size writes neither kind of target.
        let odd = "@compute @workgroup_size(2, 2, 2) fn f(@builtin(global_invocation_id) id: vec3u) {}";
        let odd = Compute::compile(&gpu, "odd", odd, &[]).unwrap();
        assert_eq!(odd.entries().collect::<Vec<_>>(), [("f", Target::Grid)]);
        assert!(odd.run(&gpu, "f", Out::Volume(&v), &reads_v).is_err());
        assert!(gpu.volume_from_atlas(&plain, [4, 4, 4]).is_err());
    }

    #[test]
    fn atlases_are_about_square_and_hold_every_slice() {
        assert_eq!(atlas_columns([128, 48, 48]), 4);
        let (w, h) = atlas_size([128, 48, 48]);
        assert!(w * h >= 128 * 48 * 48 && w <= 4 * h && h <= 4 * w, "{w}x{h}");
        assert_eq!(atlas_size([4, 4, 1]), (4, 4));
        assert_eq!(atlas_size([1, 1, 9]), (3, 3));
    }
}

#[cfg(test)]
mod buffer_tests;
#[cfg(test)]
mod liquid_tests;
