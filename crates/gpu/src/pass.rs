//! Fullscreen fragment passes: the one way `tex` nodes render, built-in and shader node alike.
//!
//! A pass is WGSL source with a fragment entry point `main`, plus declared parameters. The
//! source gets a prelude appended (WGSL declarations may come in any order, so line numbers in
//! error messages stay those of the source):
//!
//! ```wgsl
//! struct PnVertex { @builtin(position) pos: vec4f, @location(0) uv: vec2f }  // uv: 0..1, top left
//! var<uniform> pn: PnFrame;          // pn.resolution (pixels), pn.texel (1 / resolution)
//! var<uniform> pn_time: PnTime;      // pn_time.seconds, pn_time.frame (show time)
//! var<uniform> params: Params;       // one field per declared numeric parameter
//! var in0, in1, in2, in3: texture_2d<f32>;      // inputs, transparent black when unconnected
//! var pn_clamp, pn_repeat, pn_mirror: sampler;  // linear filtering, three edge modes
//! fn pn_noise(p: vec3f, seed: u32) -> f32       // gradient noise, about -1..1, integer hashes
//! fn pn_value_noise(p: vec3f, seed: u32) -> f32
//! fn pn_hash(p: vec3u) -> vec3u                 // PCG3D
//! fn pn_unpremultiply(c: vec4f) -> vec4f
//! fn pn_luma(rgb: vec3f) -> f32                 // Rec. 709 weights, linear light
//! fn pn_extend(t: texture_2d<f32>, uv: vec2f, mode: i32) -> vec4f
//!                                               // 0 hold, 1 zero, 2 repeat, 3 mirror outside 0..1
//! fn pn_area(t: texture_2d<f32>, s: sampler, uv: vec2f, footprint: vec2f) -> vec4f
//!                                               // mean over `footprint` source texels (downscaling)
//! ```
//!
//! Parameter fields: `bool` is `u32` (0 or 1), `int` and `choice` are `i32` (a choice also gets
//! a constant per entry, `const <param>_<entry>: i32`), `float` is `f32`, vectors are `vec2f` to
//! `vec4f`, a colour is a `vec4f` (straight alpha, linear). Text and file parameters have no
//! field. Whether the shader reads `pn_time` is detected, which is what makes a shader node
//! time-dependent or not.

use std::collections::hash_map::DefaultHasher;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};

use penumbra_core::{Error, ParamKind, ParamSpec, Value};

use crate::{FORMAT, Gpu, Tex};

/// Texture inputs every pass has (`in0` to `in3`).
pub const INPUTS: usize = 4;

const PRELUDE: &str = r#"
// ---- penumbra prelude ----
struct PnFrame { resolution: vec2f, texel: vec2f }
struct PnTime { seconds: f32, frame: f32, _pn0: f32, _pn1: f32 }
struct PnVertex { @builtin(position) pos: vec4f, @location(0) uv: vec2f }
@group(0) @binding(0) var<uniform> pn: PnFrame;
@group(0) @binding(1) var<uniform> pn_time: PnTime;
@group(0) @binding(2) var<uniform> params: Params;
@group(1) @binding(0) var in0: texture_2d<f32>;
@group(1) @binding(1) var in1: texture_2d<f32>;
@group(1) @binding(2) var in2: texture_2d<f32>;
@group(1) @binding(3) var in3: texture_2d<f32>;
@group(1) @binding(4) var pn_clamp: sampler;
@group(1) @binding(5) var pn_repeat: sampler;
@group(1) @binding(6) var pn_mirror: sampler;

@vertex
fn pn_vs(@builtin(vertex_index) i: u32) -> PnVertex {
    // One triangle covering the target; uv (0, 0) is the top left corner.
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    var v: PnVertex;
    v.pos = vec4f(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    v.uv = vec2f(x, y);
    return v;
}

// PCG3D (Jarzynski and Olano 2020). Integer hashes are bit-identical on every backend, unlike
// fract(sin(x) * 43758), which differs by up to 254 LSB between Vulkan and DX12.
fn pn_hash(p: vec3u) -> vec3u {
    var v = p * 1664525u + 1013904223u;
    v.x += v.y * v.z;
    v.y += v.z * v.x;
    v.z += v.x * v.y;
    v = v ^ (v >> vec3u(16u));
    v.x += v.y * v.z;
    v.y += v.z * v.x;
    v.z += v.x * v.y;
    return v;
}

fn pn_cell(c: vec3i, seed: u32) -> vec3f {
    let s = pn_hash(vec3u(seed, seed ^ 0x5bd1e995u, ~seed));
    // 24 bits per component, exactly representable: -1..1.
    return vec3f(pn_hash(bitcast<vec3u>(c) ^ s) >> vec3u(8u)) * (2.0 / 16777215.0) - 1.0;
}

fn pn_fade(t: vec3f) -> vec3f {
    return t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
}

fn pn_noise(p: vec3f, seed: u32) -> f32 {
    let i = vec3i(floor(p));
    let f = p - floor(p);
    let u = pn_fade(f);
    var n: array<f32, 8>;
    for (var k = 0u; k < 8u; k++) {
        let o = vec3i(i32(k & 1u), i32((k >> 1u) & 1u), i32(k >> 2u));
        n[k] = dot(pn_cell(i + o, seed), f - vec3f(o));
    }
    let x0 = mix(mix(n[0], n[1], u.x), mix(n[2], n[3], u.x), u.y);
    let x1 = mix(mix(n[4], n[5], u.x), mix(n[6], n[7], u.x), u.y);
    return mix(x0, x1, u.z);
}

fn pn_value_noise(p: vec3f, seed: u32) -> f32 {
    let i = vec3i(floor(p));
    let u = pn_fade(p - floor(p));
    var n: array<f32, 8>;
    for (var k = 0u; k < 8u; k++) {
        let o = vec3i(i32(k & 1u), i32((k >> 1u) & 1u), i32(k >> 2u));
        n[k] = pn_cell(i + o, seed).x;
    }
    let x0 = mix(mix(n[0], n[1], u.x), mix(n[2], n[3], u.x), u.y);
    let x1 = mix(mix(n[4], n[5], u.x), mix(n[6], n[7], u.x), u.y);
    return mix(x0, x1, u.z);
}

fn pn_unpremultiply(c: vec4f) -> vec4f {
    if (c.a <= 0.0) {
        return vec4f(0.0);
    }
    return vec4f(c.rgb / c.a, c.a);
}

fn pn_luma(rgb: vec3f) -> f32 {
    return dot(rgb, vec3f(0.2126, 0.7152, 0.0722));
}

// The edge modes of the `extend` choices: hold, zero, repeat, mirror.
fn pn_extend(t: texture_2d<f32>, uv: vec2f, mode: i32) -> vec4f {
    switch mode {
        case 1: {
            if (any(uv < vec2f(0.0)) || any(uv > vec2f(1.0))) {
                return vec4f(0.0);
            }
            return textureSampleLevel(t, pn_clamp, uv, 0.0);
        }
        case 2: {
            return textureSampleLevel(t, pn_repeat, uv, 0.0);
        }
        case 3: {
            return textureSampleLevel(t, pn_mirror, uv, 0.0);
        }
        default: {
            return textureSampleLevel(t, pn_clamp, uv, 0.0);
        }
    }
}

// Mean of the source over `footprint` texels around `uv`, so shrinking does not alias. Each
// linear tap averages up to two texels per axis, so half as many taps as texels, at most 8 x 8.
fn pn_area(t: texture_2d<f32>, s: sampler, uv: vec2f, footprint: vec2f) -> vec4f {
    let texel = 1.0 / vec2f(textureDimensions(t));
    let n = vec2i(clamp(ceil(footprint * 0.5), vec2f(1.0), vec2f(8.0)));
    var sum = vec4f(0.0);
    for (var j = 0; j < n.y; j++) {
        for (var i = 0; i < n.x; i++) {
            let o = (vec2f(f32(i), f32(j)) + 0.5) / vec2f(n) - 0.5;
            sum += textureSampleLevel(t, s, uv + o * footprint * texel, 0.0);
        }
    }
    return sum / f32(n.x * n.y);
}
"#;

/// Passes recorded to go out in one submission, for a node that runs many passes per cook.
pub struct Batch<'a> {
    gpu: &'a Gpu,
    encoder: wgpu::CommandEncoder,
}

impl<'a> Batch<'a> {
    pub fn new(gpu: &'a Gpu, label: &str) -> Self {
        let encoder =
            gpu.device().create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
        Self { gpu, encoder }
    }

    pub fn submit(self) {
        self.gpu.submit([self.encoder.finish()]);
    }
}

/// The bind group and pipeline layouts every pass shares, plus samplers and a stand-in for
/// unconnected inputs.
pub(crate) struct Layouts {
    group0: wgpu::BindGroupLayout,
    group1: wgpu::BindGroupLayout,
    pipeline: wgpu::PipelineLayout,
    samplers: [wgpu::Sampler; 3],
    empty: wgpu::TextureView,
}

impl Layouts {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let uniform = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let group0 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pass globals"),
            entries: &[uniform(0), uniform(1), uniform(2)],
        });
        let mut entries: Vec<wgpu::BindGroupLayoutEntry> = (0..INPUTS as u32)
            .map(|binding| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            })
            .collect();
        entries.extend((0..3).map(|i| wgpu::BindGroupLayoutEntry {
            binding: INPUTS as u32 + i,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        }));
        let group1 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pass inputs"),
            entries: &entries,
        });
        let pipeline = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pass"),
            bind_group_layouts: &[Some(&group0), Some(&group1)],
            immediate_size: 0,
        });
        let sampler = |mode, label| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some(label),
                address_mode_u: mode,
                address_mode_v: mode,
                address_mode_w: mode,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            })
        };
        let samplers = [
            sampler(wgpu::AddressMode::ClampToEdge, "clamp"),
            sampler(wgpu::AddressMode::Repeat, "repeat"),
            sampler(wgpu::AddressMode::MirrorRepeat, "mirror"),
        ];
        // wgpu zero-initialises resources: transparent black.
        let empty = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("unconnected input"),
                size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default());
        Self { group0, group1, pipeline, samplers, empty }
    }
}

/// Where a declared parameter lives in the `params` uniform.
#[derive(Clone, Debug)]
struct Field {
    spec: ParamSpec,
    offset: u32,
}

/// What a pass needs per render besides its target.
pub struct PassInput<'a> {
    /// Up to [`INPUTS`] textures, `None` (or missing) for unconnected ones.
    pub inputs: &'a [Option<&'a Tex>],
    pub seconds: f64,
    pub frame: i64,
    /// The value of a declared parameter; `None` (or a value the spec refuses) means its
    /// default.
    pub param: &'a dyn Fn(&str) -> Option<Value>,
}

/// naga's message with its box-drawing frame (`┌─`, `│`, `╰`) in ASCII: the UI font has no
/// glyphs for those, so they showed as boxes. Everything else is kept.
pub(crate) fn ascii_frame(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '─' | '━' | '═' | '╌' | '╍' | '┄' | '┅' | '┈' | '┉' => '-',
            '│' | '┃' | '║' | '╎' | '╏' | '┆' | '┇' | '┊' | '┋' => '|',
            '·' => ':',
            '\u{2500}'..='\u{257f}' => '+',
            c => c,
        })
        .collect()
}

/// A compiled fullscreen pass.
pub struct Pass {
    label: String,
    pipeline: wgpu::RenderPipeline,
    params: Vec<ParamSpec>,
    fields: Vec<Field>,
    uniform_size: u64,
    uses_time: bool,
    uses_input: [bool; INPUTS],
}

impl Pass {
    /// Compiles `source` (see the module docs) with `params`. The error text is meant for the
    /// person who wrote the shader: naga's message with the source line.
    pub fn compile(gpu: &Gpu, label: &str, source: &str, params: &[ParamSpec]) -> Result<Self, Error> {
        let full = format!("{source}\n{PRELUDE}{}", params_wgsl(params));
        let module = naga::front::wgsl::parse_str(&full)
            .map_err(|e| Error::new(ascii_frame(&e.emit_to_string_with_path(&full, label))))?;
        let info = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), Default::default())
            .validate(&module)
            .map_err(|e| Error::new(ascii_frame(&e.emit_to_string_with_path(&full, label))))?;
        let entry = module
            .entry_points
            .iter()
            .position(|e| e.name == "main" && e.stage == naga::ShaderStage::Fragment)
            .ok_or_else(|| {
                Error::new(format!(
                    "{label}: no fragment entry point; write `@fragment fn main(v: PnVertex) -> @location(0) vec4f`"
                ))
            })?;
        let uses = |name: &str| {
            module
                .global_variables
                .iter()
                .any(|(h, g)| g.name.as_deref() == Some(name) && !info.get_entry_point(entry)[h].is_empty())
        };
        let uses_time = uses("pn_time");
        let uses_input = [uses("in0"), uses("in1"), uses("in2"), uses("in3")];

        let (fields, uniform_size) = layout(&module, params)?;

        let device = gpu.device();
        let (pipeline, error) = gpu.scoped(|| {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(full.as_str().into()),
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&gpu.layouts().pipeline),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("pn_vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some("main"),
                    compilation_options: Default::default(),
                    targets: &[Some(FORMAT.into())],
                }),
                multiview_mask: None,
                cache: None,
            })
        });
        if let Some(e) = error {
            return Err(Error::new(format!("{label}: {e}")));
        }
        Ok(Self {
            label: label.to_string(),
            pipeline,
            params: params.to_vec(),
            fields,
            uniform_size,
            uses_time,
            uses_input,
        })
    }

    pub fn params(&self) -> &[ParamSpec] {
        &self.params
    }

    /// Whether the shader reads `pn_time`.
    pub fn uses_time(&self) -> bool {
        self.uses_time
    }

    /// Whether the shader reads input `index`.
    pub fn uses_input(&self, index: usize) -> bool {
        self.uses_input.get(index).copied().unwrap_or(false)
    }

    /// Renders into `target` (cleared first) and submits.
    pub fn render(&self, gpu: &Gpu, target: &Tex, input: &PassInput<'_>) {
        let mut batch = Batch::new(gpu, &self.label);
        self.encode(&mut batch, target, input);
        batch.submit();
    }

    /// Records rendering into `target` (cleared first) into `batch`, submitted with it.
    pub fn encode(&self, batch: &mut Batch<'_>, target: &Tex, input: &PassInput<'_>) {
        let gpu = batch.gpu;
        let encoder = &mut batch.encoder;
        let device = gpu.device();
        let layouts = gpu.layouts();
        let (w, h) = target.size();
        let frame: Vec<u8> = [w as f32, h as f32, 1.0 / w as f32, 1.0 / h as f32]
            .iter()
            .flat_map(|x| x.to_le_bytes())
            .collect();
        let time: Vec<u8> = [input.seconds as f32, input.frame as f32, 0.0, 0.0]
            .iter()
            .flat_map(|x| x.to_le_bytes())
            .collect();
        let params = self.pack(input.param);
        // One buffer for the three uniforms, each at an aligned offset: creating a buffer costs
        // more than anything else a pass does on the CPU.
        let align = device.limits().min_uniform_buffer_offset_alignment.max(16) as usize;
        let parts = [&frame, &time, &params];
        let mut offsets = [0usize; 3];
        let mut contents = Vec::new();
        for (offset, part) in offsets.iter_mut().zip(parts) {
            *offset = contents.len().next_multiple_of(align);
            contents.resize(*offset, 0);
            contents.extend_from_slice(part);
            contents.resize(*offset + part.len().max(16).next_multiple_of(16), 0);
        }
        let buffer = gpu.uniform_buffer("pass uniforms", &contents);
        let binding = |i: usize| {
            let offset = offsets.get(i).copied().unwrap_or(0);
            let size = parts.get(i).map_or(16, |p| p.len().max(16).next_multiple_of(16));
            wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer: &buffer,
                offset: offset as u64,
                size: wgpu::BufferSize::new(size as u64),
            })
        };
        let group0 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pass globals"),
            layout: &layouts.group0,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: binding(0) },
                wgpu::BindGroupEntry { binding: 1, resource: binding(1) },
                wgpu::BindGroupEntry { binding: 2, resource: binding(2) },
            ],
        });
        let view = |i: usize| match input.inputs.get(i).copied().flatten() {
            Some(t) => t.view(),
            None => &layouts.empty,
        };
        let [clamp, repeat, mirror] = &layouts.samplers;
        let group1 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pass inputs"),
            layout: &layouts.group1,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view(0)) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(view(1)) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(view(2)) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(view(3)) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(clamp) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::Sampler(repeat) },
                wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::Sampler(mirror) },
            ],
        });
        {
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(&self.label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target.view(),
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            rp.set_pipeline(&self.pipeline);
            rp.set_bind_group(0, &group0, &[]);
            rp.set_bind_group(1, &group1, &[]);
            rp.draw(0..3, 0..1);
        }
    }

    /// The `params` uniform: every declared value at its offset.
    fn pack(&self, param: &dyn Fn(&str) -> Option<Value>) -> Vec<u8> {
        let mut out = vec![0u8; usize::try_from(self.uniform_size).unwrap_or(16)];
        for f in &self.fields {
            let value = param(&f.spec.name)
                .and_then(|v| f.spec.check(&v).ok())
                .unwrap_or_else(|| f.spec.default.clone());
            let words = words(&f.spec, &value);
            for (i, word) in words.iter().enumerate() {
                let at = f.offset as usize + i * 4;
                if let Some(slot) = out.get_mut(at..at + 4) {
                    slot.copy_from_slice(word);
                }
            }
        }
        out
    }
}

/// The 4-byte words of one value, little-endian.
fn words(spec: &ParamSpec, value: &Value) -> Vec<[u8; 4]> {
    match (&spec.kind, value) {
        (ParamKind::Bool, Value::Bool(b)) => vec![u32::from(*b).to_le_bytes()],
        (ParamKind::Int, v) => {
            let i = v.as_i64().unwrap_or(0).clamp(i64::from(i32::MIN), i64::from(i32::MAX));
            vec![(i as i32).to_le_bytes()]
        }
        (ParamKind::Choice(choices), Value::Text(t)) => {
            let i = choices.iter().position(|c| c == t).unwrap_or(0);
            vec![(i32::try_from(i).unwrap_or(0)).to_le_bytes()]
        }
        (ParamKind::Float | ParamKind::Vec(_) | ParamKind::Color, v) => {
            v.components().unwrap_or(&[]).iter().map(|x| (*x as f32).to_le_bytes()).collect()
        }
        _ => Vec::new(),
    }
}

/// The WGSL type of a parameter's field; `None` for text and files.
fn wgsl_type(spec: &ParamSpec) -> Option<&'static str> {
    Some(match spec.kind {
        ParamKind::Bool => "u32",
        ParamKind::Int | ParamKind::Choice(_) => "i32",
        ParamKind::Float => "f32",
        ParamKind::Vec(2) => "vec2f",
        ParamKind::Vec(3) => "vec3f",
        ParamKind::Vec(_) | ParamKind::Color => "vec4f",
        ParamKind::Text | ParamKind::File { .. } => return None,
    })
}

/// `struct Params` and the choice constants.
fn params_wgsl(params: &[ParamSpec]) -> String {
    let mut s = String::from("struct Params {\n");
    let mut any = false;
    for p in params {
        if let Some(ty) = wgsl_type(p) {
            let _ = writeln!(s, "    {}: {ty},", p.name);
            any = true;
        }
    }
    if !any {
        // WGSL structs cannot be empty.
        s.push_str("    _pn_unused: f32,\n");
    }
    s.push_str("}\n");
    for p in params {
        if let ParamKind::Choice(choices) = &p.kind {
            for (i, c) in choices.iter().enumerate() {
                if c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
                    let _ = writeln!(s, "const {}_{c}: i32 = {i};", p.name);
                }
            }
        }
    }
    s
}

/// Field offsets and the uniform size, as naga lays out `struct Params`.
fn layout(module: &naga::Module, params: &[ParamSpec]) -> Result<(Vec<Field>, u64), Error> {
    let (members, span) = module
        .types
        .iter()
        .find_map(|(_, t)| match &t.inner {
            naga::TypeInner::Struct { members, span } if t.name.as_deref() == Some("Params") => {
                Some((members, *span))
            }
            _ => None,
        })
        .ok_or_else(|| Error::new("the shader must not declare its own `Params`"))?;
    let mut fields = Vec::new();
    for p in params.iter().filter(|p| wgsl_type(p).is_some()) {
        let m = members
            .iter()
            .find(|m| m.name.as_deref() == Some(p.name.as_str()))
            .ok_or_else(|| Error::new(format!("param `{}` is missing from `Params`", p.name)))?;
        fields.push(Field { spec: p.clone(), offset: m.offset });
    }
    // Uniform buffers are bound in multiples of 16 bytes.
    Ok((fields, u64::from(span).div_ceil(16).max(1) * 16))
}

/// Identifies a pass for [`Gpu::cached_pass`].
pub(crate) fn cache_key(source: &str, params: &[ParamSpec]) -> u64 {
    let mut h = DefaultHasher::new();
    source.hash(&mut h);
    format!("{params:?}").hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::gpu;

    const GRADIENT: &str = "
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    return vec4f(v.uv.x * params.gain, v.uv.y, params.tint.b, 1.0);
}";

    fn specs() -> Vec<ParamSpec> {
        vec![
            ParamSpec::float("gain", 1.0),
            ParamSpec::text("label", ""),
            ParamSpec::bool("flag", false),
            ParamSpec::color("tint", [0.0, 0.0, 0.25, 1.0]),
            ParamSpec::choice("mode", &["a", "b"]),
        ]
    }

    #[test]
    fn layout_follows_wgsl_alignment() {
        let full = format!("{GRADIENT}\n{PRELUDE}{}", params_wgsl(&specs()));
        assert!(full.contains("const mode_b: i32 = 1;"));
        let module = naga::front::wgsl::parse_str(&full).unwrap();
        let (fields, size) = layout(&module, &specs()).unwrap();
        let offsets: Vec<(&str, u32)> = fields.iter().map(|f| (f.spec.name.as_str(), f.offset)).collect();
        // gain f32 at 0, flag u32 at 4, tint vec4f aligned to 16, mode i32 after it.
        assert_eq!(offsets, [("gain", 0), ("flag", 4), ("tint", 16), ("mode", 32)]);
        assert_eq!(size, 48);
    }

    #[test]
    fn the_error_frame_becomes_ascii() {
        let naga = "error: no definition in scope\n  ┌─ a.wgsl:3:10\n  │\n3 │   return nope;\n  │          ^^^^ here\n  ·\n  ╰── end";
        assert_eq!(
            ascii_frame(naga),
            "error: no definition in scope\n  +- a.wgsl:3:10\n  |\n3 |   return nope;\n  |          ^^^^ here\n  :\n  +-- end"
        );
        assert_eq!(ascii_frame("ä ├┬┤"), "ä +++");
    }

    #[test]
    fn compile_errors_point_at_the_source_line() {
        let Some(gpu) = gpu() else { return };
        let e = Pass::compile(
            &gpu,
            "broken.wgsl",
            "@fragment\nfn main(v: PnVertex) -> @location(0) vec4f {\n  return nope;\n}",
            &[],
        )
        .err()
        .unwrap();
        assert!(e.0.contains("broken.wgsl:3"), "{e}");
        assert!(e.0.is_ascii(), "{e}");
        let e = Pass::compile(&gpu, "x", "fn f() {}", &[]).err().unwrap();
        assert!(e.0.contains("no fragment entry point"), "{e}");
    }

    #[test]
    fn renders_and_detects_what_it_reads() {
        let Some(gpu) = gpu() else { return };
        let pass = Pass::compile(&gpu, "gradient", GRADIENT, &specs()).unwrap();
        assert!(!pass.uses_time());
        assert!(!pass.uses_input(0));
        let target = Tex::new(&gpu, 4, 2, "t").unwrap();
        let gain = |name: &str| (name == "gain").then_some(Value::Float(2.0));
        pass.render(&gpu, &target, &PassInput { inputs: &[], seconds: 0.0, frame: 0, param: &gain });
        let px = gpu.read(&target).unwrap();
        // Pixel centres: x = 0.125, 0.375, ...; times gain 2.
        let [r, g, b, a] = px.get(1, 1).unwrap();
        assert!((r - 0.75).abs() < 1e-3 && (g - 0.75).abs() < 1e-3 && (b - 0.25).abs() < 1e-3 && a == 1.0);
        let timed = Pass::compile(
            &gpu,
            "timed",
            "@fragment fn main(v: PnVertex) -> @location(0) vec4f { return textureSample(in1, pn_clamp, v.uv) + pn_time.seconds; }",
            &[],
        )
        .unwrap();
        assert!(timed.uses_time() && timed.uses_input(1) && !timed.uses_input(0));
        assert_eq!(gpu.errors().count, 0);
    }
}
