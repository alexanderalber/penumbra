//! Volumes (3D textures) and compute programs that read and write them: what a simulation in
//! three dimensions and a ray-march need, beside the fragment passes of [`crate::pass`].
//!
//! A [`Volume`] is a grid of cells of [`VOLUME_FORMAT`] (RGBA, 16-bit float), filtered linearly
//! when sampled. A [`Compute`] program is WGSL with compute entry points; each writes one target,
//! a volume or a picture (a [`Tex`] made by [`Tex::new_storage`]). The entry point's workgroup size
//! says which: `@workgroup_size(4, 4, 4)` writes a volume, `@workgroup_size(8, 8, 1)` a picture.
//! A dispatch covers the target, so a shader returns early for ids outside `pn.size`.
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

use crate::pass::{self, Field, SHARED};
use crate::{Batch, FORMAT, Gpu, Tex};

/// The format of every volume's cells.
pub const VOLUME_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Most cells of a volume: 64 MiB of half floats. A simulation keeps a few volumes of its grid.
pub const MAX_VOLUME_CELLS: u64 = 1 << 23;

/// Volume inputs a program has (`vol0` to `vol3`), and picture inputs (`pic0`, `pic1`).
pub const VOLUME_INPUTS: usize = 4;
pub const PICTURE_INPUTS: usize = 2;

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

/// A volume as it flows between nodes (`vol` data): the cells and the box they fill, in metres.
/// Channel use by convention: red the smoke's density, green its temperature, blue the fuel,
/// alpha free.
#[derive(Clone)]
pub struct Medium {
    pub volume: Volume,
    /// The box's centre and its size along x, y and z.
    pub center: [f32; 3],
    pub size: [f32; 3],
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
    Volume,
    Picture,
}

impl Target {
    fn workgroup(self) -> [u32; 3] {
        match self {
            Target::Volume => [4, 4, 4],
            Target::Picture => [8, 8, 1],
        }
    }
}

/// The target of one dispatch.
#[derive(Clone, Copy)]
pub enum Out<'a> {
    Volume(&'a Volume),
    Picture(&'a Tex),
}

impl Out<'_> {
    fn target(&self) -> Target {
        match self {
            Out::Volume(_) => Target::Volume,
            Out::Picture(_) => Target::Picture,
        }
    }

    fn size(&self) -> [u32; 3] {
        match self {
            Out::Volume(v) => v.size(),
            Out::Picture(t) => [t.width(), t.height(), 1],
        }
    }
}

/// The inputs and values of one dispatch.
pub struct Dispatch<'a> {
    pub volumes: &'a [Option<&'a Volume>],
    pub pictures: &'a [Option<&'a Tex>],
    pub seconds: f64,
    pub frame: i64,
    pub param: &'a dyn Fn(&str) -> Option<Value>,
}

struct Entry {
    name: String,
    target: Target,
    pipeline: wgpu::ComputePipeline,
}

/// A compiled compute program: its entry points, sharing one set of parameters.
pub struct Compute {
    label: String,
    entries: Vec<Entry>,
    fields: Vec<Field>,
    uniform_size: u64,
    layouts: Arc<Layouts>,
}

impl Compute {
    /// Compiles `source` with the prelude. `Err` with the compiler's message for WGSL that does
    /// not compile, and for an entry point whose workgroup size is neither a volume's nor a
    /// picture's.
    pub fn compile(gpu: &Gpu, label: &str, source: &str, params: &[ParamSpec]) -> Result<Self, Error> {
        let full = format!("{source}\n{PRELUDE}{SHARED}{}", pass::params_wgsl(params));
        let module = naga::front::wgsl::parse_str(&full)
            .map_err(|e| Error::new(pass::ascii_frame(&e.emit_to_string_with_path(&full, label))))?;
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), Default::default())
            .validate(&module)
            .map_err(|e| Error::new(pass::ascii_frame(&e.emit_to_string_with_path(&full, label))))?;
        let (fields, uniform_size) = pass::layout(&module, params)?;
        let layouts = gpu.compute_layouts();
        let mut wanted = Vec::new();
        for e in module.entry_points.iter().filter(|e| e.stage == naga::ShaderStage::Compute) {
            let target = match e.workgroup_size {
                [4, 4, 4] => Target::Volume,
                [8, 8, 1] => Target::Picture,
                [x, y, z] => {
                    return Err(Error::new(format!(
                        "{label}: entry point `{}` has workgroup size ({x}, {y}, {z}); write (4, 4, 4) for \
                         one that writes a volume, (8, 8, 1) for one that writes a picture",
                        e.name
                    )));
                }
            };
            wanted.push((e.name.clone(), target));
        }
        if wanted.is_empty() {
            return Err(Error::new(format!("{label}: no compute entry point")));
        }
        let device = gpu.device();
        let (entries, error) = gpu.scoped(|| {
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(full.as_str().into()),
            });
            wanted
                .into_iter()
                .map(|(name, target)| {
                    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                        label: Some(&format!("{label} {name}")),
                        layout: Some(&layouts.pipeline),
                        module: &shader,
                        entry_point: Some(&name),
                        compilation_options: Default::default(),
                        cache: None,
                    });
                    Entry { name, target, pipeline }
                })
                .collect::<Vec<_>>()
        });
        if let Some(e) = error {
            return Err(Error::new(format!("{label}: {e}")));
        }
        Ok(Self { label: label.to_string(), entries, fields, uniform_size, layouts })
    }

    /// The names of the entry points and what each writes.
    pub fn entries(&self) -> impl Iterator<Item = (&str, Target)> {
        self.entries.iter().map(|e| (e.name.as_str(), e.target))
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
        self.record(batch.gpu(), &[Step { entry, out, input }])
    }

    /// Records `steps` in turn into one command encoder: as [`Compute::encode`] for each, at a
    /// fraction of the CPU's cost per step. A step reads what the steps before it wrote. `Err`
    /// as for one step; then none of them runs.
    pub fn encode_steps(&self, batch: &mut Batch<'_>, steps: &[Step<'_>]) -> Result<(), Error> {
        self.record(batch.gpu(), steps)
    }

    /// Runs entry point `entry` writing `out`: submitted at once, unless the calling thread
    /// defers ([`Gpu::defer`]).
    pub fn run(&self, gpu: &Gpu, entry: &str, out: Out<'_>, input: &Dispatch<'_>) -> Result<(), Error> {
        self.record(gpu, &[Step { entry, out, input }])
    }

    fn record(&self, gpu: &Gpu, steps: &[Step<'_>]) -> Result<(), Error> {
        struct Ready<'e> {
            entry: &'e Entry,
            groups: [u32; 3],
            parts: [Vec<u8>; 3],
        }
        let mut ready = Vec::with_capacity(steps.len());
        for step in steps {
            let (entry, out, input) = (step.entry, step.out, step.input);
            let e = self
                .entries
                .iter()
                .find(|e| e.name == entry)
                .ok_or_else(|| Error::new(format!("{}: no entry point `{entry}`", self.label)))?;
            if e.target != out.target() {
                return Err(Error::new(format!(
                    "{}: entry point `{entry}` writes a {:?}, not a {:?}",
                    self.label,
                    e.target,
                    out.target()
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
            let mut time = vec![0u8; 16];
            for (slot, x) in
                time.as_chunks_mut::<4>().0.iter_mut().zip([input.seconds as f32, input.frame as f32])
            {
                slot.copy_from_slice(&x.to_le_bytes());
            }
            let params = pass::pack(&self.fields, self.uniform_size, input.param);
            let wg = e.target.workgroup();
            let groups: [u32; 3] = std::array::from_fn(|i| {
                size.get(i).copied().unwrap_or(1).div_ceil(wg.get(i).copied().unwrap_or(1))
            });
            ready.push(Ready { entry: e, groups, parts: [grid, time, params] });
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
            for ((r, step), slots) in ready.iter().zip(steps).zip(slots) {
                let group1 = self.layouts.group(gpu, step.input, step.out);
                cp.set_pipeline(&r.entry.pipeline);
                cp.set_bind_group(0, &slots.group, &slots.offsets);
                cp.set_bind_group(1, &group1, &[]);
                cp.dispatch_workgroups(r.groups[0], r.groups[1], r.groups[2]);
            }
        });
        match error {
            Some(e) => {
                let names: Vec<&str> = steps.iter().map(|s| s.entry).collect();
                Err(Error::new(format!("{} {}: {e}", self.label, names.join(", "))))
            }
            None => Ok(()),
        }
    }
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
    /// The last bind groups made, with what they bind, for reuse while all of it lives.
    groups: std::sync::Mutex<Vec<Group>>,
}

struct Group {
    volumes: [Option<VolumeKey>; VOLUME_INPUTS],
    pictures: [Option<crate::tex::TexKey>; PICTURE_INPUTS],
    out: OutKey,
    group: wgpu::BindGroup,
}

enum OutKey {
    Volume(VolumeKey),
    Picture(crate::tex::TexKey),
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
        let out = match (&self.out, out) {
            (OutKey::Volume(k), Out::Volume(v)) => k.is(v),
            (OutKey::Picture(k), Out::Picture(t)) => k.is(t),
            _ => false,
        };
        volumes && pictures && out
    }

    fn alive(&self) -> bool {
        let out = match &self.out {
            OutKey::Volume(k) => k.0.strong_count() > 0,
            OutKey::Picture(k) => k.alive(),
        };
        out && self.volumes.iter().flatten().all(|k| k.0.strong_count() > 0)
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
            groups: std::sync::Mutex::new(Vec::new()),
        }
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
            ],
        });
        kept.push(Group {
            volumes: std::array::from_fn(|i| input.volumes.get(i).copied().flatten().map(Volume::key)),
            pictures: std::array::from_fn(|i| input.pictures.get(i).copied().flatten().map(Tex::key)),
            out: match out {
                Out::Volume(v) => OutKey::Volume(v.key()),
                Out::Picture(t) => OutKey::Picture(t.key()),
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
        let input =
            Dispatch { volumes: &[Some(volume)], pictures: &[], seconds: 0.0, frame: 0, param: &param };
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
        let input =
            Dispatch { volumes: &[], pictures: &[Some(atlas)], seconds: 0.0, frame: 0, param: &param };
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
        let none = Dispatch { volumes: &[], pictures: &[], seconds: 0.0, frame: 0, param: &param };
        program.run(&gpu, "fill", Out::Volume(&a), &none).unwrap();
        let from_a = Dispatch { volumes: &[Some(&a)], pictures: &[], seconds: 0.0, frame: 0, param: &param };
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
        let reads_v = Dispatch { volumes: &[Some(&v)], pictures: &[], seconds: 0.0, frame: 0, param: &param };
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
        let odd = "@compute @workgroup_size(2, 2, 2) fn f(@builtin(global_invocation_id) id: vec3u) {}";
        assert!(Compute::compile(&gpu, "odd", odd, &[]).is_err());
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
