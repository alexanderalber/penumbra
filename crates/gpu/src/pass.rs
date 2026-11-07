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
//! fn pn_resample(t: texture_2d<f32>, uv: vec2f, footprint: vec2f, mode: i32, edge: i32) -> vec4f
//!                                               // scaling: 0 linear, 1 cubic, 2 nearest;
//!                                               // edge as in pn_extend (zero holds here)
//! struct PnPalette { count: i32, ... }           // a palette param's field; read it with these
//! fn pn_palette(p: PnPalette, i: i32) -> vec4f  // colour i, wrapping (also below 0)
//! fn pn_colormap(p: PnPalette, t: f32) -> vec4f // 0..1 along the colours, blended
//! fn pn_band(p: PnPalette, t: f32) -> vec4f     // 0..1 in equal bands, one per colour
//! ```
//!
//! Parameter fields: `bool` is `u32` (0 or 1), `int` and `choice` are `i32` (a choice also gets
//! a constant per entry, `const <param>_<entry>: i32`), `float` is `f32`, vectors are `vec2f` to
//! `vec4f`, a colour is a `vec4f` (straight alpha, linear), a palette a `PnPalette` (its
//! colours linear, straight alpha; `count` 0 for an empty one, whose colours are all zero).
//! Text and file parameters have no field. Whether the shader reads `pn_time` is detected, which is what makes a shader node
//! time-dependent or not.

use std::collections::hash_map::DefaultHasher;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use penumbra_core::{Error, ParamKind, ParamSpec, Value, lock, palette};

use crate::tex::TexKey;
use crate::{Deferral, Done, FORMAT, Gpu, Tex};

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
"#;

/// The hashes and noise every shader gets, fragment passes and compute programs alike.
pub(crate) const SHARED: &str = r#"
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
"#;

const PRELUDE_REST: &str = r#"
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
    return pn_box(t, s, uv, footprint, 8.0);
}

// The source at `uv` for an output pixel `footprint` source texels wide. `mode` 0 linear
// (bilinear growing, the mean of the footprint shrinking), 1 cubic (Catmull-Rom, its kernel
// stretched over the footprint shrinking); both count every texel up to 512 texels per pixel
// (cubic 128) and skip some beyond. 2 nearest (the one texel under the pixel, hard edges, aliases when shrinking).
// `edge` is pn_extend's numbering (zero holds), applied per tap, so a caller inlines this once,
// not once per sampler: three copies made a shader WARP took longer to run than a read waits.
// Ifs, not a switch whose every case returns: FXC (Direct3D) rejects that as a path
// without a return.
fn pn_resample(t: texture_2d<f32>, uv: vec2f, footprint: vec2f, mode: i32, edge: i32) -> vec4f {
    let size = vec2f(textureDimensions(t));
    if (mode == 1) {
        let x = uv * size;
        let n = vec2i(pn_cubic_count(footprint.x), pn_cubic_count(footprint.y));
        // A tap stands for the texels up to the next: the mean of them.
        let span = vec2f(pn_cubic_step(footprint.x), pn_cubic_step(footprint.y));
        var sum = vec4f(0.0);
        var weight = 0.0;
        for (var j = 0; j < n.y; j++) {
            let ty = pn_cubic_tap(x.y, footprint.y, j);
            for (var i = 0; i < n.x; i++) {
                let tx = pn_cubic_tap(x.x, footprint.x, i);
                let w = tx.y * ty.y;
                sum += w * pn_box_edge(t, edge, vec2f(tx.x, ty.x) / size, span);
                weight += w;
            }
        }
        sum /= weight;
        // The kernel's negative lobes overshoot at hard edges: no negative light, alpha 0..1.
        return vec4f(max(sum.rgb, vec3f(0.0)), clamp(sum.a, 0.0, 1.0));
    }
    if (mode == 2) {
        // A texel's centre, where linear filtering returns that texel alone.
        return pn_tap(t, edge, (floor(uv * size) + 0.5) / size);
    }
    if (all(footprint <= vec2f(16.0))) {
        return pn_box_edge(t, edge, uv, footprint);
    }
    // Beyond 16 texels a row of boxes, each an even number of whole texels wide, so its taps sit
    // on texel edges and average exactly; the row is snapped to whole texels.
    let part = 2.0 * ceil(footprint / 32.0);
    let n = vec2i(clamp(round(footprint / part), vec2f(1.0), vec2f(32.0)));
    let start = round(uv * size - vec2f(n) * part * 0.5);
    var sum = vec4f(0.0);
    for (var j = 0; j < n.y; j++) {
        for (var i = 0; i < n.x; i++) {
            let at = start + (vec2f(f32(i), f32(j)) + 0.5) * part;
            sum += pn_box_edge(t, edge, at / size, part);
        }
    }
    return sum / f32(n.x * n.y);
}

fn pn_box(t: texture_2d<f32>, s: sampler, uv: vec2f, footprint: vec2f, most: f32) -> vec4f {
    let texel = 1.0 / vec2f(textureDimensions(t));
    let n = vec2i(clamp(ceil(footprint * 0.5), vec2f(1.0), vec2f(most)));
    var sum = vec4f(0.0);
    for (var j = 0; j < n.y; j++) {
        for (var i = 0; i < n.x; i++) {
            let o = (vec2f(f32(i), f32(j)) + 0.5) / vec2f(n) - 0.5;
            sum += textureSampleLevel(t, s, uv + o * footprint * texel, 0.0);
        }
    }
    return sum / f32(n.x * n.y);
}

// pn_box with up to 8 x 8 taps, through the sampler `edge` names.
fn pn_box_edge(t: texture_2d<f32>, edge: i32, uv: vec2f, footprint: vec2f) -> vec4f {
    let texel = 1.0 / vec2f(textureDimensions(t));
    let n = vec2i(clamp(ceil(footprint * 0.5), vec2f(1.0), vec2f(8.0)));
    var sum = vec4f(0.0);
    for (var j = 0; j < n.y; j++) {
        for (var i = 0; i < n.x; i++) {
            let o = (vec2f(f32(i), f32(j)) + 0.5) / vec2f(n) - 0.5;
            sum += pn_tap(t, edge, uv + o * footprint * texel);
        }
    }
    return sum / f32(n.x * n.y);
}

// One linear sample with `edge`'s edge mode: 2 repeat, 3 mirror, else held. One sampler and
// selects, no branch: branches in the nested loops of pn_resample blew the shader up. Mirrored
// or held, a point is moved inside the outer texels' centres, where the repeating sampler
// blends nothing from the other side, which is what a mirroring or clamping sampler returns.
fn pn_tap(t: texture_2d<f32>, edge: i32, uv: vec2f) -> vec4f {
    let half = 0.5 / vec2f(textureDimensions(t));
    let mirrored = 1.0 - abs(fract(uv * 0.5) * 2.0 - 1.0);
    let inside = clamp(select(uv, mirrored, edge == 3), half, 1.0 - half);
    return textureSampleLevel(t, pn_repeat, select(inside, uv, edge == 2), 0.0);
}

// Catmull-Rom weight at distance `x`, in kernel units; 0 from 2 on.
fn pn_catmull_rom(x: f32) -> f32 {
    let a = abs(x);
    if (a < 1.0) {
        return (1.5 * a - 2.5) * a * a + 1.0;
    }
    if (a < 2.0) {
        return ((-0.5 * a + 2.5) * a - 4.0) * a + 2.0;
    }
    return 0.0;
}

// Catmull-Rom taps along one axis for an output pixel `k` source texels wide. The kernel is 4
// texels wide growing and stretched to 4k shrinking. Up to 8 texels per pixel a tap sits on
// every texel's centre. Beyond, taps sit on texel edges an even number of texels apart, each
// the mean of the texels up to the next (linear filtering averages the two at an edge
// equally): each texel counts once, up to 33 taps.
fn pn_cubic_count(k: f32) -> i32 {
    return i32(ceil(4.0 * max(k, 1.0) / pn_cubic_step(k))) + 1;
}

fn pn_cubic_step(k: f32) -> f32 {
    if (k <= 8.0) {
        return 1.0;
    }
    return 2.0 * ceil(k / 16.0);
}

// Tap `i` at `x` (texels from the edge): its position in texels and its weight, not yet
// normalised.
fn pn_cubic_tap(x: f32, k: f32, i: i32) -> vec2f {
    let unit = max(k, 1.0);
    let step = pn_cubic_step(k);
    let p = floor(x - 2.0 * unit) + step * (f32(i) + 0.5);
    return vec2f(p, pn_catmull_rom((p - x) / unit));
}

// A palette param: `count` colours, linear, straight alpha. Passed by value, so it is small: its
// colours stay in the uniform and `first` says where they start. The functions that read them
// come with `params` (`PALETTE_FUNCTIONS`).
struct PnPalette { count: i32, first: i32, _pn1: i32, _pn2: i32 }
"#;

/// The functions that read a palette's colours. Only a shader with a palette param gets them,
/// because `_pn_colours` exists only then. The colours are indexed in the uniform itself:
/// copying them into a `var` for every call costs more than the rest of many shaders.
const PALETTE_FUNCTIONS: &str = r#"
fn pn_palette(p: PnPalette, i: i32) -> vec4f {
    let n = clamp(p.count, 1, 32);
    return params._pn_colours[p.first + ((i % n) + n) % n];
}

fn pn_colormap(p: PnPalette, t: f32) -> vec4f {
    let n = clamp(p.count, 1, 32);
    let x = clamp(t, 0.0, 1.0) * f32(n - 1);
    let i = clamp(i32(floor(x)), 0, n - 1);
    let j = min(i + 1, n - 1);
    return mix(params._pn_colours[p.first + i], params._pn_colours[p.first + j], x - f32(i));
}

fn pn_band(p: PnPalette, t: f32) -> vec4f {
    let n = clamp(p.count, 1, 32);
    return params._pn_colours[p.first + clamp(i32(floor(clamp(t, 0.0, 1.0) * f32(n))), 0, n - 1)];
}
"#;

/// Passes recorded to go out together, at the latest when [`Batch::submit`] is called: for a
/// node that wants its passes on the GPU at once rather than at the end of the cook pass (a
/// simulation starting a long job, say), or that runs on a thread of its own. Inside the cook
/// pass plain [`Pass::render`] calls are collected anyway. Dropped without [`Batch::submit`],
/// the passes go out with the next submission.
pub struct Batch<'a> {
    gpu: &'a Gpu,
    label: String,
    _deferral: Deferral<'a>,
}

impl<'a> Batch<'a> {
    pub fn new(gpu: &'a Gpu, label: &str) -> Self {
        Self { gpu, label: label.to_string(), _deferral: gpu.defer() }
    }

    /// Submits the passes, after everything recorded before them, and tells without blocking
    /// when they have finished on the GPU.
    pub(crate) fn gpu(&self) -> &'a Gpu {
        self.gpu
    }

    pub fn submit(self) -> Done {
        self.gpu.done()
    }
}

/// The bind group and pipeline layouts every pass shares, plus samplers and a stand-in for
/// unconnected inputs.
pub(crate) struct Layouts {
    /// The three uniforms, at dynamic offsets into a block of uniform memory ([`crate::deferred`]).
    pub(crate) group0: wgpu::BindGroupLayout,
    group1: wgpu::BindGroupLayout,
    pipeline: wgpu::PipelineLayout,
    samplers: [wgpu::Sampler; 3],
    empty: wgpu::TextureView,
    /// Bytes between the starts of two uniforms in a block.
    pub(crate) uniform_align: u64,
}

impl Layouts {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        // Compute programs ([`crate::compute`]) share these globals.
        let uniform = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT.union(wgpu::ShaderStages::COMPUTE),
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: true,
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
        let uniform_align = u64::from(device.limits().min_uniform_buffer_offset_alignment.max(16));
        Self { group0, group1, pipeline, samplers, empty, uniform_align }
    }
}

/// Where a declared parameter lives in the `params` uniform.
#[derive(Clone, Debug)]
pub(crate) struct Field {
    spec: ParamSpec,
    offset: u32,
    /// For a palette: its number among the shader's palettes, and where its colours start.
    palette: Option<(u32, u32)>,
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
    /// Bind groups of the inputs used lately: making one costs about a fifth of a pass.
    inputs: Arc<InputGroups>,
}

/// The input bind groups of one [`Pass`]. [`Gpu::poll`] sweeps out the dead ones now and then
/// ([`Gpu::sweep_input_groups`]), also of passes no node uses any more.
pub(crate) type InputGroups = Mutex<Vec<InputGroup>>;

/// Input bind groups a [`Pass`] keeps. Built-in passes are shared by every node of their kind,
/// and each node's inputs usually circulate through two or three textures.
const INPUT_GROUPS: usize = 32;

/// The bind group of one set of inputs, `None` where unconnected.
pub(crate) struct InputGroup {
    inputs: [Option<TexKey>; INPUTS],
    group: wgpu::BindGroup,
}

impl InputGroup {
    fn matches(&self, inputs: &[Option<&Tex>]) -> bool {
        self.inputs.iter().enumerate().all(|(i, key)| match (key, inputs.get(i).copied().flatten()) {
            (None, None) => true,
            (Some(k), Some(t)) => k.is(t),
            _ => false,
        })
    }

    /// Whether every texture it binds is still held by someone besides the bind group.
    pub(crate) fn alive(&self) -> bool {
        self.inputs.iter().flatten().all(TexKey::alive)
    }
}

impl Pass {
    /// Compiles `source` (see the module docs) with `params`. The error text is meant for the
    /// person who wrote the shader: naga's message with the source line.
    pub fn compile(gpu: &Gpu, label: &str, source: &str, params: &[ParamSpec]) -> Result<Self, Error> {
        let full = format!("{source}\n{PRELUDE}{SHARED}{PRELUDE_REST}{}", params_wgsl(params));
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
            inputs: gpu.input_groups(),
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

    /// Renders into `target` (cleared first). Submits at once, unless the calling thread
    /// defers ([`Gpu::defer`]). A validation error of the work (a target of the wrong format,
    /// say) is returned instead of reaching wgpu's global handler, so the node reports it.
    pub fn render(&self, gpu: &Gpu, target: &Tex, input: &PassInput<'_>) -> Result<(), Error> {
        match self.record(gpu, target, input) {
            Some(e) => Err(Error::new(format!("{}: {e}", self.label))),
            None => Ok(()),
        }
    }

    /// Records rendering into `target` (cleared first) as part of `batch`. An error is logged.
    pub fn encode(&self, batch: &mut Batch<'_>, target: &Tex, input: &PassInput<'_>) {
        if let Some(e) = self.record(batch.gpu, target, input) {
            log::error!("{} ({}): {e}", self.label, batch.label);
        }
    }

    fn record(&self, gpu: &Gpu, target: &Tex, input: &PassInput<'_>) -> Option<String> {
        let (w, h) = target.size();
        let words = |v: [f32; 4]| {
            let mut out = [0u8; 16];
            for (slot, x) in out.as_chunks_mut::<4>().0.iter_mut().zip(v) {
                slot.copy_from_slice(&x.to_le_bytes());
            }
            out
        };
        let frame = words([w as f32, h as f32, 1.0 / w as f32, 1.0 / h as f32]);
        let time = words([input.seconds as f32, input.frame as f32, 0.0, 0.0]);
        let params = self.pack(input.param);
        gpu.record(&self.label, [&frame, &time, &params], |encoder, slots| {
            let group1 = self.input_group(gpu, input.inputs);
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
            rp.set_bind_group(0, &slots.group, &slots.offsets);
            rp.set_bind_group(1, &group1, &[]);
            rp.draw(0..3, 0..1);
        })
    }

    /// The bind group of `inputs`, made or kept. Groups whose textures are gone otherwise are
    /// let go, so the cache keeps no texture alive for long.
    fn input_group(&self, gpu: &Gpu, inputs: &[Option<&Tex>]) -> wgpu::BindGroup {
        let mut kept = lock(&self.inputs);
        if let Some(i) = kept.iter().position(|g| g.matches(inputs)) {
            let g = kept.remove(i);
            let group = g.group.clone();
            kept.push(g);
            return group;
        }
        kept.retain(InputGroup::alive);
        if kept.len() >= INPUT_GROUPS {
            kept.remove(0);
        }
        let layouts = gpu.layouts();
        let view = |i: usize| match inputs.get(i).copied().flatten() {
            Some(t) => t.view(),
            None => &layouts.empty,
        };
        let [clamp, repeat, mirror] = &layouts.samplers;
        let group = gpu.device().create_bind_group(&wgpu::BindGroupDescriptor {
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
        let keys = std::array::from_fn(|i| inputs.get(i).copied().flatten().map(Tex::key));
        kept.push(InputGroup { inputs: keys, group: group.clone() });
        group
    }

    /// The `params` uniform: every declared value at its offset.
    fn pack(&self, param: &dyn Fn(&str) -> Option<Value>) -> Vec<u8> {
        pack(&self.fields, self.uniform_size, param)
    }
}

/// The `params` uniform of `size` bytes: every field's value at its offset, the default where
/// `param` has none or one that does not fit.
pub(crate) fn pack(fields: &[Field], size: u64, param: &dyn Fn(&str) -> Option<Value>) -> Vec<u8> {
    let mut out = vec![0u8; usize::try_from(size).unwrap_or(16)];
    for f in fields {
        let value =
            param(&f.spec.name).and_then(|v| f.spec.check(&v).ok()).unwrap_or_else(|| f.spec.default.clone());
        if let Some((number, colours_at)) = f.palette {
            let (head, colours) = palette_words(&value, number);
            put(&mut out, f.offset, &head);
            put(&mut out, colours_at, &colours);
        } else {
            put(&mut out, f.offset, &words(&f.spec, &value));
        }
    }
    out
}

fn put(out: &mut [u8], offset: u32, words: &[[u8; 4]]) {
    for (i, word) in words.iter().enumerate() {
        let at = offset as usize + i * 4;
        if let Some(slot) = out.get_mut(at..at + 4) {
            slot.copy_from_slice(word);
        }
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

/// A palette's words: its `PnPalette` (the count, the index of its first colour, two of padding)
/// and its [`palette::MAX_COLOURS`] colours. Names the built-in set does not know (the project's
/// own) resolve to nothing here: the node puts those in as hex colours before the pass reads them.
fn palette_words(value: &Value, number: u32) -> (Vec<[u8; 4]>, Vec<[u8; 4]>) {
    let colours = palette::resolve(value, &[]).ok().flatten().map(|p| p.colours).unwrap_or_default();
    let count = i32::try_from(colours.len().min(palette::MAX_COLOURS)).unwrap_or(0);
    let first = i32::try_from(number as usize * palette::MAX_COLOURS).unwrap_or(0);
    let head = vec![count.to_le_bytes(), first.to_le_bytes(), [0; 4], [0; 4]];
    let mut out = Vec::with_capacity(palette::MAX_COLOURS * 4);
    for i in 0..palette::MAX_COLOURS {
        let c = colours.get(i).copied().unwrap_or([0.0; 4]);
        out.extend(c.iter().map(|x| (*x as f32).to_le_bytes()));
    }
    (head, out)
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
        ParamKind::Palette => "PnPalette",
        ParamKind::Text | ParamKind::File { .. } => return None,
    })
}

/// `struct Params`, the choice constants and, with palettes, the functions that read them.
pub(crate) fn params_wgsl(params: &[ParamSpec]) -> String {
    let palettes = params.iter().filter(|p| matches!(p.kind, ParamKind::Palette)).count();
    let mut s = String::from("struct Params {\n");
    let mut any = false;
    for p in params {
        if let Some(ty) = wgsl_type(p) {
            // A struct in a uniform starts on a multiple of 16 bytes.
            let align = if matches!(p.kind, ParamKind::Palette) { "@align(16) " } else { "" };
            let _ = writeln!(s, "    {align}{}: {ty},", p.name);
            any = true;
        }
    }
    if !any {
        // WGSL structs cannot be empty.
        s.push_str("    _pn_unused: f32,\n");
    }
    if palettes > 0 {
        let _ = writeln!(s, "    _pn_colours: array<vec4f, {}>,", palettes * palette::MAX_COLOURS);
    }
    s.push_str("}\n");
    if palettes > 0 {
        s.push_str(PALETTE_FUNCTIONS);
    }
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
pub(crate) fn layout(module: &naga::Module, params: &[ParamSpec]) -> Result<(Vec<Field>, u64), Error> {
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
    let colours = members.iter().find(|m| m.name.as_deref() == Some("_pn_colours")).map_or(0, |m| m.offset);
    let mut palettes = 0u32;
    let mut fields = Vec::new();
    for p in params.iter().filter(|p| wgsl_type(p).is_some()) {
        let m = members
            .iter()
            .find(|m| m.name.as_deref() == Some(p.name.as_str()))
            .ok_or_else(|| Error::new(format!("param `{}` is missing from `Params`", p.name)))?;
        let palette = matches!(p.kind, ParamKind::Palette).then(|| {
            let number = palettes;
            palettes += 1;
            let bytes_per_palette = u32::try_from(palette::MAX_COLOURS * 16).unwrap_or(u32::MAX);
            (number, colours.saturating_add(number.saturating_mul(bytes_per_palette)))
        });
        fields.push(Field { spec: p.clone(), offset: m.offset, palette });
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
        let full = format!("{GRADIENT}\n{PRELUDE}{SHARED}{PRELUDE_REST}{}", params_wgsl(&specs()));
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
        pass.render(&gpu, &target, &PassInput { inputs: &[], seconds: 0.0, frame: 0, param: &gain }).unwrap();
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

    #[test]
    fn palettes_reach_the_shader() {
        let Some(gpu) = gpu() else { return };
        let src = "@fragment fn main(v: PnVertex) -> @location(0) vec4f {
            if (v.uv.y < 0.5) { return pn_palette(params.p, i32(v.uv.x * 4.0) + 4 * params.n); }
            return vec4f(pn_colormap(params.p, v.uv.x).rgb, f32(params.p.count) / 8.0);
        }";
        let specs = [ParamSpec::int("n", 0), ParamSpec::palette("p", "")];
        let pass = Pass::compile(&gpu, "palette", src, &specs).unwrap();
        let target = Tex::new(&gpu, 4, 2, "t").unwrap();
        let value = |name: &str| match name {
            "p" => Some(Value::Text("ff0000 00ff00 0000ff ffffff".into())),
            "n" => Some(Value::Int(-3)),
            _ => None,
        };
        pass.render(&gpu, &target, &PassInput { inputs: &[], seconds: 0.0, frame: 0, param: &value })
            .unwrap();
        let px = gpu.read(&target).unwrap();
        // Top row: colours 0..3, shifted by -12 (wraps to the same).
        assert_eq!(px.get(0, 0).unwrap(), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(px.get(2, 0).unwrap(), [0.0, 0.0, 1.0, 1.0]);
        // Bottom row: x = 0.375 is 1.125 of the way, between green and blue; alpha the count / 8.
        let [r, g, b, a] = px.get(1, 1).unwrap();
        assert!(r.abs() < 1e-3 && (g - 0.875).abs() < 2e-3 && (b - 0.125).abs() < 2e-3, "{r} {g} {b}");
        assert!((a - 0.5).abs() < 1e-3);
        // A built-in name, and an empty palette (count 0).
        let blood = |name: &str| (name == "p").then(|| Value::Text("blood".into()));
        pass.render(&gpu, &target, &PassInput { inputs: &[], seconds: 0.0, frame: 0, param: &blood })
            .unwrap();
        let [r, ..] = gpu.read(&target).unwrap().get(0, 0).unwrap();
        assert!((r - palette::to_linear(f64::from(0x31) / 255.0) as f32).abs() < 1e-3);
        let none = |_: &str| None;
        pass.render(&gpu, &target, &PassInput { inputs: &[], seconds: 0.0, frame: 0, param: &none }).unwrap();
        assert_eq!(gpu.read(&target).unwrap().get(0, 1).unwrap()[3], 0.0);
        assert_eq!(gpu.errors().count, 0);
    }

    /// Each palette finds its own colours wherever it sits among the params.
    #[test]
    fn several_palettes_keep_their_own_colours() {
        let Some(gpu) = gpu() else { return };
        let src = "@fragment fn main(v: PnVertex) -> @location(0) vec4f {
            switch i32(v.uv.x * 4.0) {
                case 0: { return pn_palette(params.a, -1); }
                case 1: { return pn_colormap(params.b, 1.0); }
                case 2: { return pn_band(params.c, 0.0); }
                default: {
                    return vec4f(f32(params.a.count), f32(params.b.count), f32(params.c.count), params.gain) / 4.0;
                }
            }
        }";
        let specs = [
            ParamSpec::palette("a", ""),
            ParamSpec::float("gain", 1.0),
            ParamSpec::palette("b", ""),
            ParamSpec::palette("c", ""),
        ];
        let pass = Pass::compile(&gpu, "palettes", src, &specs).unwrap();
        let target = Tex::new(&gpu, 4, 1, "t").unwrap();
        let value = |name: &str| match name {
            "a" => Some(Value::Text("ff0000 00ff00".into())),
            "b" => Some(Value::Text("00ff00 0000ff ff0000".into())),
            "c" => Some(Value::Text("0000ff".into())),
            "gain" => Some(Value::Float(2.0)),
            _ => None,
        };
        pass.render(&gpu, &target, &PassInput { inputs: &[], seconds: 0.0, frame: 0, param: &value })
            .unwrap();
        let px = gpu.read(&target).unwrap();
        // Colour -1 of a, wrapped: the last of its two.
        assert_eq!(px.get(0, 0).unwrap(), [0.0, 1.0, 0.0, 1.0]);
        assert_eq!(px.get(1, 0).unwrap(), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(px.get(2, 0).unwrap(), [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(px.get(3, 0).unwrap(), [0.5, 0.75, 0.25, 0.5]);
        assert_eq!(gpu.errors().count, 0);
    }
}
