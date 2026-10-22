//! Renders [`Geo`] into textures: a depth-tested colour pass, multisampled and resolved into a
//! [`Tex`], and optionally a data pass for shaders and masks downstream (texture coordinates in red
//! and green, the part id plus one in blue, 0 where nothing was drawn, the distance along the view
//! in metres in alpha). Half floats hold part ids exactly up to 2048.
//!
//! The camera is a world-to-clip matrix (`calib::Camera::clip_from_world`, so a calibrated
//! projector renders exactly as it was solved) plus the view's depth row and the eye.
//!
//! For `res.analyze` a render can also measure: a third pass, at most [`MEASURE_SIDE`] pixels
//! across, writes the derivatives of the texture coordinates per pixel into a float texture that is
//! read back without waiting ([`Measurement`]). The colour pass can draw the resolution overlay
//! ([`Look::overlay`]) from the same derivatives, per pixel.
//!
//! Lights are [`Light`]s passed with each render, at most [`MAX_LIGHTS`]; only [`Shading::Phong`]
//! and [`Shading::Pbr`] materials answer them. No light is added by itself: phong without lights
//! is black.
//!
//! Wireframes ([`crate::geo::Wire`]) draw each triangle's corners on their own, with barycentric
//! coordinates, from a buffer the model makes on first use; the lines lie inside the triangles,
//! so an outline edge whose neighbour faces are seen edge-on shows at half the width. Occluders
//! ([`Shading::Occluder`]) draw before everything else, writing depth but no colour.
//!
//! Projector lights and shadows: a projector light carries its own world-to-clip matrix and a
//! picture it throws onto the geometry (at most [`MAX_PROJECTORS`] pictures per render).
//! Directional, spot and projector lights can cast shadows: before the colour pass, a depth-only
//! pass per such light (at most [`MAX_SHADOWS`]) draws the geometry from the light into one layer
//! of a depth texture array, leaving out what is transparent (alpha below one half); the colour
//! pass compares against it with 3x3 percentage-closer filtering, the point moved along its normal
//! by about a shadow texel against acne.
//!
//! See-through surfaces and the environment light: the colour pass draws opaque pixels first, then
//! the see-through ones back to front by their draw's middle, back faces before front faces; lights
//! casting shadows also keep a tint map of what see-through surfaces let through. One environment
//! light per render ([`Environment`]) lights phong and pbr materials from its baked maps.
//!
//! Ambient occlusion ([`Ao`]): before the colour pass, a pass without multisampling writes the
//! opaque surfaces' positions and normals; a fullscreen pass (`ao.wgsl`) estimates per pixel how
//! much of the hemisphere above the surface the geometry around it hides, then a blur over the
//! sampling pattern's 4x4 cell removes its noise. The colour pass dims only ambient and
//! environment light by it: direct light and what a surface emits are left alone, and so is
//! every unlit surface (projection content keeps its colours).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use penumbra_core::{Error, lock};
use penumbra_mesh::Bounds;

use crate::environment::EnvironmentMaps;
use crate::geo::{Faces, Geo, Instances, Mat4, Material, Shading, VERTEX_BYTES, WIRE_BYTES};
use crate::projection::Overlay;
use crate::{FORMAT, Gpu, Tex};

/// The shader, after the helpers the built-in shaders share.
const SOURCE: &str = concat!(include_str!("common.wgsl"), include_str!("render.wgsl"));

#[cfg(test)]
mod layout_tests;
mod passes;

use passes::{Calls, Encoding, MeasureTargets, RenderPipes, Shared, Wanted};

const DEPTH: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// The measuring pass's format: four derivatives at full precision.
const MEASURE: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;
/// The measuring pass's longest side. Enough for a 95th percentile (130 000 samples at 16:9)
/// and small enough to read back and sort in a few milliseconds.
pub const MEASURE_SIDE: u32 = 480;
/// Most lights one render draws.
pub const MAX_LIGHTS: usize = 16;
/// Most lights casting shadows in one render; further ones light without.
pub const MAX_SHADOWS: usize = 8;
/// Most projector lights with a picture in one render; further ones are left out.
pub const MAX_PROJECTORS: usize = 4;
const LIGHT_BYTES: u64 = 128;
const FRAME_BYTES: u64 = 144 + MAX_LIGHTS as u64 * LIGHT_BYTES;
const DRAW_BYTES: u64 = 224;
const AO_BYTES: u64 = 96;
/// Most draws (runs of one part and material, over all models) in one render.
pub const MAX_DRAWS: usize = 1 << 16;

/// Where the camera looks from, as matrices.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    /// World to clip, row-major for column vectors (wgpu's clip space: depth 0..1, y up).
    pub clip_from_world: Mat4,
    /// The view's depth row: `dot(view_z, [x, y, z, 1])` is the distance along the view.
    pub view_z: [f64; 4],
    /// Where the camera is.
    pub eye: [f64; 3],
    /// An orthographic camera: every ray runs along the view (`view_z`'s direction), so shading
    /// takes that one direction to the viewer instead of the way to `eye`.
    pub parallel: bool,
}

/// What kind of light a [`Light`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightKind {
    /// From everywhere alike: lights every surface, also where it faces away.
    Ambient,
    /// From far away along one direction, as the sun.
    Directional,
    /// From a point, in every direction.
    Point,
    /// From a point, in a cone.
    Spot,
    /// From a point, through a camera's frustum, throwing a picture ([`Projector`]).
    Projector,
    /// From a picture of everything around ([`Environment`]).
    Environment,
}

/// What makes a light an environment light: its baked picture and how it is turned.
#[derive(Clone, Debug, PartialEq)]
pub struct Environment {
    pub maps: Arc<EnvironmentMaps>,
    /// Degrees about z, counter-clockwise seen from above.
    pub turn: f64,
}

/// What makes a light a projector: where its picture lands and the picture.
#[derive(Clone)]
pub struct Projector {
    /// World to the projector's clip space (as [`View::clip_from_world`]); its picture fills
    /// -1..1 in x and y.
    pub clip_from_world: Mat4,
    /// Linear, premultiplied; `None` throws white.
    pub texture: Option<Tex>,
}

impl std::fmt::Debug for Projector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Projector")
            .field("clip_from_world", &self.clip_from_world)
            .field("texture", &self.texture.as_ref().map(Tex::size))
            .finish()
    }
}

impl PartialEq for Projector {
    fn eq(&self, other: &Self) -> bool {
        self.clip_from_world == other.clip_from_world
            && match (&self.texture, &other.texture) {
                (Some(a), Some(b)) => a.same(b),
                (None, None) => true,
                _ => false,
            }
    }
}

/// One light, in world coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct Light {
    pub kind: LightKind,
    /// Where it is (point, spot).
    pub position: [f64; 3],
    /// Where it shines to (directional, spot); need not be of unit length.
    pub direction: [f64; 3],
    /// Linear, times its intensity.
    pub color: [f32; 3],
    /// Spot: the cosines of half the cone's angle and of where the light starts to fade
    /// towards the edge.
    pub cone: [f32; 2],
    /// Point, spot and projector: fades with the square of the distance (the colour holds at
    /// one metre), else the same at any distance.
    pub inverse_square: bool,
    /// Directional, spot and projector: casts shadows.
    pub shadows: bool,
    /// The shadow's bias, times the usual: more against acne (stripes on lit surfaces), less
    /// against light leaking where objects touch.
    pub bias: f32,
    /// Projector lights only.
    pub projector: Option<Projector>,
    /// Environment lights only.
    pub environment: Option<Environment>,
}

impl Light {
    pub fn ambient(color: [f32; 3]) -> Self {
        Self {
            kind: LightKind::Ambient,
            position: [0.0; 3],
            direction: [0.0, 0.0, -1.0],
            color,
            cone: [-1.0, -1.0],
            inverse_square: false,
            shadows: false,
            bias: 1.0,
            projector: None,
            environment: None,
        }
    }

    pub fn point(position: [f64; 3], color: [f32; 3]) -> Self {
        Self { kind: LightKind::Point, position, ..Self::ambient(color) }
    }
}

/// The payload of `light` data: the lights a node passes on.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lights(pub Vec<Light>);

/// What the colour pass shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    /// The geometry's [`Material`]; without one each part's material from the file (colour
    /// times texture), unlit.
    Material,
    /// The node's input texture, by the texture coordinates, shaded as the geometry's
    /// [`Material`] says.
    Texture,
    /// A colour per part id.
    Parts,
    /// Texture coordinates as red and green.
    Uv,
    /// Normals as colours.
    Normals,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    pub surface: Surface,
    /// Constant materials answer the lights as phong ones without a highlight do: for the
    /// pictures `geo` and `mat` nodes show of themselves, where shape matters more than colour.
    pub shade_all: bool,
    /// Linear, premultiplied.
    pub background: [f32; 4],
    /// Leave out triangles facing away (counter-clockwise is the front).
    pub cull: bool,
    /// Four samples per pixel.
    pub antialias: bool,
    /// The resolution overlay over the input texture ([`Surface::Texture`] only).
    pub overlay: Option<Overlay>,
    /// Texels across and down each shadow map; 0 draws no shadows at all.
    pub shadow_size: u32,
    /// Ambient occlusion; `None` draws none.
    pub ao: Option<Ao>,
}

/// Ambient occlusion: ambient and environment light dimmed where nearby geometry hides much of
/// the surroundings (corners, creases, where objects meet).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ao {
    /// How far around a point geometry counts, in metres.
    pub radius: f32,
    /// How much of the hidden ambient light is taken away: 1 as estimated, 0.3 at most 30 %
    /// even where everything around is hidden; above 1 darker, down to black.
    pub strength: f32,
}

/// Which of the passes a pipeline draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Pass {
    /// The colour pass's opaque pixels, writing depth.
    Solid,
    /// The colour pass's see-through pixels, drawn back to front over the solid ones without
    /// writing depth.
    Blend,
    Data,
    Measure,
    /// Depth only, from a light; see-through pixels left out.
    Shadow,
    /// From a light: what its see-through surfaces let through, multiplied, and the nearest
    /// one's depth.
    Tint,
    /// For ambient occlusion: the opaque pixels' positions and normals.
    Prepass,
}

/// How a pipeline draws within its pass: as usual, or the two steps that draw wireframes with
/// hidden faces last, so that only the lines a filled face would leave show.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Stage {
    Draw,
    /// The faces' depth only.
    Hide,
    /// The lines where that depth is the nearest.
    Over,
}

impl Pass {
    fn format(self) -> wgpu::TextureFormat {
        if matches!(self, Pass::Measure | Pass::Prepass) { MEASURE } else { FORMAT }
    }

    fn entry(self) -> &'static str {
        match self {
            Pass::Solid => "fs_solid",
            Pass::Blend => "fs_blend",
            Pass::Data => "fs_data",
            Pass::Measure => "fs_measure",
            Pass::Shadow => "fs_shadow",
            Pass::Tint => "fs_tint",
            Pass::Prepass => "fs_prepass",
        }
    }

    fn colour(self) -> bool {
        matches!(self, Pass::Solid | Pass::Blend)
    }

    fn blend(self) -> Option<wgpu::BlendState> {
        match self {
            Pass::Solid | Pass::Blend => Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
            // Colour: what is there times what this surface lets through; alpha: the nearest
            // depth.
            Pass::Tint => Some(wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::Zero,
                    dst_factor: wgpu::BlendFactor::Src,
                    operation: wgpu::BlendOperation::Add,
                },
                alpha: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::One,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Min,
                },
            }),
            _ => None,
        }
    }
}

/// Alpha from which a pixel counts as opaque: above 254/255, so 8-bit textures' opaque texels
/// are, and below one, so half floats' rounding cannot push an opaque one out.
pub const OPAQUE: f32 = 0.998;

/// The size of the measuring pass for a render of `size`: the same aspect, at most
/// [`MEASURE_SIDE`] across or down.
pub fn measure_size(size: (u32, u32)) -> (u32, u32) {
    let long = size.0.max(size.1).max(1);
    if long <= MEASURE_SIDE {
        return (size.0.max(1), size.1.max(1));
    }
    let k = f64::from(MEASURE_SIDE) / f64::from(long);
    let side = |s: u32| ((f64::from(s) * k).round() as u32).clamp(1, MEASURE_SIDE);
    (side(size.0), side(size.1))
}

/// The derivatives of the texture coordinates per pixel of a measuring pass, on their way to
/// the CPU. Never waits: [`Measurement::take`] answers `None` until the GPU has delivered,
/// which happens during a later [`Gpu::poll`].
pub struct Measurement {
    buffer: wgpu::Buffer,
    size: (u32, u32),
    padded: u64,
    /// 0 waiting, 1 mapped, 2 failed, 3 taken.
    state: Arc<AtomicU8>,
}

/// One pixel of a measurement: the texture coordinates' change per pixel of the measuring
/// pass, `[du/dx, dv/dx, du/dy, dv/dy]` (x right, y down). Zero where nothing was drawn.
pub type Derivatives = [f32; 4];

impl Measurement {
    /// The measuring pass's size in pixels.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// The pixels, row by row from the top, once the GPU has delivered them; `None` before.
    /// Gives them once; afterwards an error.
    pub fn take(&self) -> Option<Result<Vec<Derivatives>, Error>> {
        match self.state.load(Ordering::Acquire) {
            0 => return None,
            1 => {}
            2 => return Some(Err(Error::new("cannot map the measurement"))),
            _ => return Some(Err(Error::new("the measurement was taken already"))),
        }
        self.state.store(3, Ordering::Release);
        let (w, h) = (self.size.0 as usize, self.size.1 as usize);
        let row = w * 16;
        let out = (|| {
            let view =
                self.buffer.get_mapped_range(..).map_err(|e| Error::new(format!("measurement: {e}")))?;
            let mut data = Vec::with_capacity(w * h);
            for y in 0..h {
                let start = y * self.padded as usize;
                let line =
                    view.get(start..start + row).ok_or_else(|| Error::new("the measurement is too short"))?;
                for px in line.as_chunks::<16>().0 {
                    let (floats, _) = px.as_chunks::<4>();
                    let mut d = [0f32; 4];
                    for (o, f) in d.iter_mut().zip(floats) {
                        *o = f32::from_le_bytes(*f);
                    }
                    data.push(d);
                }
            }
            Ok(data)
        })();
        self.buffer.unmap();
        Some(out)
    }
}

/// Samples, pass, faces left out, wireframe, stage.
type PipelineKey = (u32, Pass, Option<wgpu::Face>, bool, Stage);

/// A pass's pipeline for whole surfaces, and for wireframes where the render draws any.
struct Pipes {
    plain: wgpu::RenderPipeline,
    wire: Option<wgpu::RenderPipeline>,
}

/// Pipelines and layouts, one set per device ([`Gpu::render_pipelines`]).
pub struct Pipelines {
    frame_layout: wgpu::BindGroupLayout,
    draw_layout: wgpu::BindGroupLayout,
    tex_layout: wgpu::BindGroupLayout,
    /// The colour pass's fourth group: shadow maps and projector pictures.
    light_layout: wgpu::BindGroupLayout,
    module: wgpu::ShaderModule,
    /// Frame, draw, texture: the data, measuring and shadow passes.
    layout: wgpu::PipelineLayout,
    /// ... and the light group: the colour pass.
    color_layout: wgpu::PipelineLayout,
    /// By (samples, pass, faces left out, wireframe, stage).
    built: std::sync::Mutex<HashMap<PipelineKey, wgpu::RenderPipeline>>,
    /// Whether the texture format allows four samples.
    msaa: bool,
    /// Ambient occlusion: its uniforms and textures, the estimate and the blur.
    ao_layout: wgpu::BindGroupLayout,
    ao_estimate: wgpu::RenderPipeline,
    ao_blur: wgpu::RenderPipeline,
}

/// The bind group layouts of the render shader: frame, draw, texture, and the colour pass's
/// lights.
struct Layouts {
    frame: wgpu::BindGroupLayout,
    draw: wgpu::BindGroupLayout,
    tex: wgpu::BindGroupLayout,
    light: wgpu::BindGroupLayout,
}

impl Layouts {
    fn new(device: &wgpu::Device) -> Self {
        let frame = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("render frame"),
            entries: &[
                uniform_entry(false, FRAME_BYTES),
                sampler_entry(1, wgpu::SamplerBindingType::Filtering),
            ],
        });
        let draw = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("render draw"),
            entries: &[uniform_entry(true, DRAW_BYTES)],
        });
        let tex = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("render texture"),
            entries: &[
                picture_entry(0),
                instance_entry(1),
                instance_entry(2),
                instance_entry(3),
                instance_entry(4),
            ],
        });
        let light = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("render lights"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                sampler_entry(1, wgpu::SamplerBindingType::Comparison),
                picture_entry(2),
                picture_entry(3),
                picture_entry(4),
                picture_entry(5),
                sampler_entry(6, wgpu::SamplerBindingType::Filtering),
                picture_entry(8),
                picture_entry(9),
                sampler_entry(10, wgpu::SamplerBindingType::Filtering),
                loaded_entry(11),
                wgpu::BindGroupLayoutEntry {
                    binding: 7,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        Self { frame, draw, tex, light }
    }
}

/// A uniform buffer at binding 0, for both stages.
fn uniform_entry(dynamic: bool, size: u64) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding: 0,
        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: dynamic,
            min_binding_size: wgpu::BufferSize::new(size),
        },
        count: None,
    }
}

/// A sampled picture for the fragment stage.
fn picture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

/// A texture read with textureLoad only, so any float format fits (the positions are 32-bit).
fn loaded_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

/// An instance texture, read by the vertex stage with textureLoad (any float format).
fn instance_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry { visibility: wgpu::ShaderStages::VERTEX, ..loaded_entry(binding) }
}

fn sampler_entry(binding: u32, ty: wgpu::SamplerBindingType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(ty),
        count: None,
    }
}

/// Ambient occlusion's estimate and blur, full-screen passes over `ao_layout`.
fn ao_pipelines(
    device: &wgpu::Device,
    ao_layout: &wgpu::BindGroupLayout,
) -> (wgpu::RenderPipeline, wgpu::RenderPipeline) {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("render ao"),
        source: wgpu::ShaderSource::Wgsl(include_str!("ao.wgsl").into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("render ao"),
        bind_group_layouts: &[Some(ao_layout)],
        immediate_size: 0,
    });
    let pipeline = |entry: &str| {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(entry),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        })
    };
    (pipeline("fs_estimate"), pipeline("fs_blur"))
}

impl Pipelines {
    pub(crate) fn new(gpu: &Gpu) -> Result<Self, Error> {
        let device = gpu.device();
        let ((layouts, module, layout, color_layout, ao_layout), error) = gpu.scoped(|| {
            let layouts = Layouts::new(device);
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("render"),
                source: wgpu::ShaderSource::Wgsl(SOURCE.into()),
            });
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("render"),
                bind_group_layouts: &[Some(&layouts.frame), Some(&layouts.draw), Some(&layouts.tex)],
                immediate_size: 0,
            });
            let color_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("render colour"),
                bind_group_layouts: &[
                    Some(&layouts.frame),
                    Some(&layouts.draw),
                    Some(&layouts.tex),
                    Some(&layouts.light),
                ],
                immediate_size: 0,
            });
            let ao_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("render ao"),
                entries: &[uniform_entry(false, AO_BYTES), loaded_entry(1), loaded_entry(2), loaded_entry(3)],
            });
            (layouts, module, layout, color_layout, ao_layout)
        });
        if let Some(e) = error {
            return Err(Error::new(format!("cannot build the render shader: {e}")));
        }
        let ((ao_estimate, ao_blur), error) = gpu.scoped(|| ao_pipelines(device, &ao_layout));
        if let Some(e) = error {
            return Err(Error::new(format!("cannot build the ambient occlusion shader: {e}")));
        }
        let msaa = gpu.adapter().get_texture_format_features(FORMAT).flags.sample_count_supported(4)
            && gpu.adapter().get_texture_format_features(DEPTH).flags.sample_count_supported(4);
        let Layouts { frame, draw, tex, light } = layouts;
        Ok(Self {
            frame_layout: frame,
            draw_layout: draw,
            tex_layout: tex,
            light_layout: light,
            module,
            layout,
            color_layout,
            built: std::sync::Mutex::new(HashMap::new()),
            msaa,
            ao_layout,
            ao_estimate,
            ao_blur,
        })
    }

    fn pipeline(
        &self,
        gpu: &Gpu,
        samples: u32,
        pass: Pass,
        cull: Option<wgpu::Face>,
        wire: bool,
        stage: Stage,
    ) -> Result<wgpu::RenderPipeline, Error> {
        let key = (samples, pass, cull, wire, stage);
        let mut built = lock(&self.built);
        if let Some(p) = built.get(&key) {
            return Ok(p.clone());
        }
        let attributes = [
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 0, shader_location: 0 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 12, shader_location: 1 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 24, shader_location: 2 },
            // Wireframes only: barycentric coordinates and the edges inside flat polygons.
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 32, shader_location: 3 },
        ];
        let (entry, stride, attributes) = if wire {
            ("vs_wire", WIRE_BYTES, &attributes[..])
        } else {
            ("vs", VERTEX_BYTES, &attributes[..3])
        };
        let target = |format: wgpu::TextureFormat| {
            // Hidden faces write depth only.
            let write_mask =
                if stage == Stage::Hide { wgpu::ColorWrites::empty() } else { wgpu::ColorWrites::ALL };
            Some(wgpu::ColorTargetState { format, blend: pass.blend(), write_mask })
        };
        let fragment = match (stage, pass) {
            (Stage::Hide, _) => "fs_hide",
            // The colour pass's lines keep nothing back: the faces between give alpha 0.
            (Stage::Over, Pass::Solid) => "fs_over",
            _ => pass.entry(),
        };
        let shadow = pass == Pass::Shadow;
        let targets = match pass {
            Pass::Shadow => Vec::new(),
            // Positions and distance along the view, then normals.
            Pass::Prepass => vec![target(MEASURE), target(FORMAT)],
            _ => vec![target(pass.format())],
        };
        let (pipeline, error) = gpu.scoped(|| {
            gpu.device().create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(pass.entry()),
                layout: Some(if pass.colour() { &self.color_layout } else { &self.layout }),
                vertex: wgpu::VertexState {
                    module: &self.module,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: stride,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes,
                    })],
                },
                primitive: wgpu::PrimitiveState { cull_mode: cull, ..Default::default() },
                // The tint pass has no depth: every see-through surface along the ray counts.
                depth_stencil: (pass != Pass::Tint).then_some(wgpu::DepthStencilState {
                    format: DEPTH,
                    depth_write_enabled: Some(pass != Pass::Blend),
                    // The lines over hidden faces pass where the faces' own depth is.
                    depth_compare: Some(if stage == Stage::Over {
                        wgpu::CompareFunction::LessEqual
                    } else {
                        wgpu::CompareFunction::Less
                    }),
                    stencil: wgpu::StencilState::default(),
                    // Against acne where a surface is seen at a slant from the light; the rest is
                    // the colour pass's normal offset.
                    bias: if shadow {
                        wgpu::DepthBiasState { constant: 2, slope_scale: 1.5, clamp: 0.0 }
                    } else {
                        wgpu::DepthBiasState::default()
                    },
                }),
                multisample: wgpu::MultisampleState { count: samples, ..Default::default() },
                fragment: Some(wgpu::FragmentState {
                    module: &self.module,
                    entry_point: Some(fragment),
                    compilation_options: Default::default(),
                    targets: &targets,
                }),
                multiview_mask: None,
                cache: None,
            })
        });
        if let Some(e) = error {
            return Err(Error::new(format!("cannot build the render pipeline: {e}")));
        }
        built.insert(key, pipeline.clone());
        Ok(pipeline)
    }
}

impl Gpu {
    /// The render pipelines, built once per device.
    pub fn render_pipelines(&self) -> Result<Arc<Pipelines>, Error> {
        let mut slot = lock(&self.inner.render);
        if let Some(p) = slot.as_ref() {
            return Ok(p.clone());
        }
        let p = Arc::new(Pipelines::new(self)?);
        *slot = Some(p.clone());
        Ok(p)
    }
}

/// The attachments one render node keeps between frames (depth and multisampled targets of
/// its size) and its uniform buffers.
pub struct Renderer {
    size: (u32, u32),
    msaa_color: Option<wgpu::TextureView>,
    depth: Option<(u32, wgpu::TextureView)>,
    depth_data: Option<wgpu::TextureView>,
    frame: Option<wgpu::Buffer>,
    draws: Option<wgpu::Buffer>,
    white: Option<Tex>,
    sampler: Option<wgpu::Sampler>,
    shadows: Option<ShadowMaps>,
    /// What see-through surfaces let through, per light casting shadows; 1x1 while none is
    /// drawn.
    tints: Option<ShadowMaps>,
    /// The shadow passes' frame uniforms, one per layer.
    shadow_frames: Vec<wgpu::Buffer>,
    /// Shadow maps' comparing sampler, the projector pictures' (clamped at the edges) and the
    /// environment's (around in u).
    light_samplers: Option<(wgpu::Sampler, wgpu::Sampler, wgpu::Sampler)>,
    /// Ambient occlusion's targets, of the render's size, and its uniforms.
    ao: Option<AoTargets>,
    ao_uniforms: Option<wgpu::Buffer>,
}

/// What ambient occlusion draws into: the prepass's positions (and distance along the view) and
/// normals, the noisy estimate and the blurred one the colour pass reads (visibility in red, the
/// distance along the view in green).
struct AoTargets {
    positions: wgpu::TextureView,
    normals: wgpu::TextureView,
    estimate: wgpu::TextureView,
    blurred: wgpu::TextureView,
}

impl AoTargets {
    fn new(gpu: &Gpu, size: (u32, u32)) -> Self {
        let target = |format: wgpu::TextureFormat, label: &str| {
            gpu.device()
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        Self {
            positions: target(MEASURE, "ao positions"),
            normals: target(FORMAT, "ao normals"),
            estimate: target(FORMAT, "ao estimate"),
            blurred: target(FORMAT, "ao"),
        }
    }
}

/// A texture array, one layer per light casting shadows: depth (the shadow maps), or what
/// see-through surfaces let through (the tint maps: colour, and the nearest one's depth in
/// alpha).
struct ShadowMaps {
    size: u32,
    layers: u32,
    array: wgpu::TextureView,
    /// One per layer, to render into.
    layer_views: Vec<wgpu::TextureView>,
}

impl ShadowMaps {
    fn new(gpu: &Gpu, size: u32, layers: u32, format: wgpu::TextureFormat) -> Self {
        let texture = gpu.device().create_texture(&wgpu::TextureDescriptor {
            label: Some(if format == DEPTH { "shadow maps" } else { "tint maps" }),
            size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: layers },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let array = texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let layer_views = (0..layers)
            .map(|k| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_array_layer: k,
                    array_layer_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        Self { size, layers, array, layer_views }
    }
}

/// What the colour pass needs of a light beyond its numbers.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Placed {
    /// World to the light's clip space: its shadow map's and a projector's picture.
    clip: Mat4,
    /// Its layer in the shadow maps.
    layer: Option<u32>,
    /// Which of the projector pictures it throws.
    slot: Option<u32>,
    /// A projector beyond [`MAX_PROJECTORS`] pictures, or an environment light after the first:
    /// left out.
    dropped: bool,
}

/// Places the lights: shadow layers for those casting shadows (while `shadows`, the first
/// [`MAX_SHADOWS`] whose shadow can be framed), picture slots for the first [`MAX_PROJECTORS`]
/// projectors with a picture; environment lights after the first are left out. Shadows are
/// framed around `bounds`.
fn place_lights(lights: &[Light], bounds: Option<&Bounds>, shadows: bool) -> Vec<Placed> {
    let (mut layers, mut slots, mut environment) = (0u32, 0u32, false);
    lights
        .iter()
        .map(|l| {
            let mut p = Placed { clip: crate::geo::IDENTITY, layer: None, slot: None, dropped: false };
            if l.environment.is_some() {
                p.dropped = environment;
                environment = true;
            }
            if let Some(proj) = &l.projector {
                p.clip = proj.clip_from_world;
                if proj.texture.is_some() {
                    if (slots as usize) < MAX_PROJECTORS {
                        p.slot = Some(slots);
                        slots += 1;
                    } else {
                        p.dropped = true;
                    }
                }
            }
            if shadows
                && l.shadows
                && !p.dropped
                && (layers as usize) < MAX_SHADOWS
                && let Some(clip) = shadow_clip(l, bounds)
            {
                p.clip = clip;
                p.layer = Some(layers);
                layers += 1;
            }
            p
        })
        .collect()
}

/// World to the clip space of `light`'s shadow map: a projector's own, a spot's cone (its
/// depth range around `bounds`), a box around `bounds` along a directional light. `None` for
/// lights without shadows, or when nothing is there to cast one.
fn shadow_clip(light: &Light, bounds: Option<&Bounds>) -> Option<Mat4> {
    if light.kind == LightKind::Projector {
        return light.projector.as_ref().map(|p| p.clip_from_world);
    }
    let b = bounds?;
    let c = b.center().map(f64::from);
    let s = b.size().map(f64::from);
    let r = (0.5 * (s[0] * s[0] + s[1] * s[1] + s[2] * s[2]).sqrt()).max(1e-4) * 1.02;
    let f = unit(light.direction)?;
    // Any two axes across the light; which way round does not matter for a depth map.
    let up = if f[2].abs() > 0.99 { [0.0, 1.0, 0.0] } else { [0.0, 0.0, 1.0] };
    let x = unit(cross(f, up))?;
    let y = cross(x, f);
    let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let row = |a: [f64; 3], k: f64, at: [f64; 3]| [a[0] * k, a[1] * k, a[2] * k, -dot(a, at) * k];
    let m = match light.kind {
        LightKind::Directional => {
            // Depth 0 to 1 across the sphere around the bounds.
            let z = row(f, 0.5 / r, c);
            [row(x, 1.0 / r, c), row(y, 1.0 / r, c), [z[0], z[1], z[2], z[3] + 0.5], [0.0, 0.0, 0.0, 1.0]]
        }
        LightKind::Spot => {
            let eye = light.position;
            let d = dot(f, [c[0] - eye[0], c[1] - eye[1], c[2] - eye[2]]);
            let far = d + r;
            if far <= 0.0 {
                return None;
            }
            let near = (d - r).max(far * 1e-4);
            let half = f64::from(light.cone[0].clamp(-1.0, 1.0)).acos().clamp(1e-3, 85f64.to_radians());
            let k = 1.0 / half.tan();
            let a = far / (far - near);
            let z = row(f, a, eye);
            [row(x, k, eye), row(y, k, eye), [z[0], z[1], z[2], z[3] - a * near], row(f, 1.0, eye)]
        }
        _ => return None,
    };
    m.iter().flatten().all(|v| v.is_finite()).then_some(m)
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn unit(v: [f64; 3]) -> Option<[f64; 3]> {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    (len > 1e-12 && len.is_finite()).then(|| v.map(|x| x / len))
}

impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderer {
    pub fn new() -> Self {
        Self {
            size: (0, 0),
            msaa_color: None,
            depth: None,
            depth_data: None,
            frame: None,
            draws: None,
            white: None,
            sampler: None,
            shadows: None,
            tints: None,
            shadow_frames: Vec::new(),
            light_samplers: None,
            ao: None,
            ao_uniforms: None,
        }
    }

    fn attachment(
        gpu: &Gpu,
        size: (u32, u32),
        format: wgpu::TextureFormat,
        samples: u32,
    ) -> wgpu::TextureView {
        gpu.device()
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("render attachment"),
                size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default())
    }

    /// Draws `geo` seen through `view` and lit by `lights` (the first [`MAX_LIGHTS`]) into
    /// `color`, and into `data` when given (same size). `texture` is the input texture for
    /// [`Surface::Texture`].
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        gpu: &Gpu,
        geo: &Geo,
        lights: &[Light],
        view: &View,
        look: &Look,
        texture: Option<&Tex>,
        color: &Tex,
        data: Option<&Tex>,
    ) -> Result<(), Error> {
        self.draw(gpu, geo, lights, view, look, texture, color, data, false).map(|_| ())
    }

    /// [`Renderer::render`], and in the same submission the measuring pass: the derivatives of
    /// the texture coordinates per pixel, at [`measure_size`], on their way to the CPU.
    #[allow(clippy::too_many_arguments)]
    pub fn render_and_measure(
        &mut self,
        gpu: &Gpu,
        geo: &Geo,
        lights: &[Light],
        view: &View,
        look: &Look,
        texture: Option<&Tex>,
        color: &Tex,
        data: Option<&Tex>,
    ) -> Result<Measurement, Error> {
        self.draw(gpu, geo, lights, view, look, texture, color, data, true)?
            .ok_or_else(|| Error::new("the measurement could not be started"))
    }

    #[allow(clippy::too_many_arguments)]
    fn draw(
        &mut self,
        gpu: &Gpu,
        geo: &Geo,
        lights: &[Light],
        view: &View,
        look: &Look,
        texture: Option<&Tex>,
        color: &Tex,
        data: Option<&Tex>,
        measure: bool,
    ) -> Result<Option<Measurement>, Error> {
        let pipelines = gpu.render_pipelines()?;
        let size = color.size();
        if data.is_some_and(|d| d.size() != size) {
            return Err(Error::new("the data texture must have the colour texture's size"));
        }
        let lights = lights.get(..MAX_LIGHTS).unwrap_or(lights);
        let placed = place_lights(lights, geo.bounds().as_ref(), look.shadow_size > 0);
        let layers = placed.iter().filter(|p| p.layer.is_some()).count() as u32;
        let ao = ambient_occlusion(look, lights, &placed);
        let align = u64::from(gpu.device().limits().min_uniform_buffer_offset_alignment).max(DRAW_BYTES);
        let calls = Calls::new(geo, view, look, texture, align.div_ceil(256).max(1) * 256)?;
        let tinted = layers > 0 && !calls.tinting.is_empty();
        let wanted = Wanted {
            samples: if look.antialias && pipelines.msaa { 4 } else { 1 },
            data: data.is_some(),
            measure,
            shadows: layers > 0,
            tinted,
            ao: ao.is_some(),
        };
        let pipes = RenderPipes::new(gpu, &pipelines, look, &calls, wanted)?;
        let measuring = if measure { Some(MeasureTargets::new(gpu, size)?) } else { None };
        self.ensure_targets(gpu, size, wanted)?;
        let wires = calls.wires(gpu, geo)?;
        self.ensure_draws(gpu, calls.bytes.len() as u64);
        // Shadow maps: a 1x1 layer stands in while no light casts a shadow. Tint maps at half
        // their size (what passes through glass is soft anyway), 1x1 while nothing is
        // see-through.
        let shadow_size = if layers > 0 { look.shadow_size.clamp(1, gpu.max_texture_size()) } else { 1 };
        let tint_size = if tinted { (shadow_size / 2).max(1) } else { 1 };
        self.ensure_maps(gpu, shadow_size, tint_size, layers, tinted)?;
        let shared = self.shared(gpu)?;

        let maps = Maps { shadow: shadow_size, tint: tint_size, tinted, ao: ao.is_some() };
        for (p, buffer) in placed.iter().filter_map(|p| p.layer.map(|_| p)).zip(&self.shadow_frames) {
            let from = View { clip_from_world: p.clip, view_z: [0.0; 4], eye: [0.0; 3], parallel: false };
            gpu.queue().write_buffer(buffer, 0, &frame_bytes(&from, look, maps, &[], &[]));
        }
        gpu.queue().write_buffer(&shared.frame, 0, &frame_bytes(view, look, maps, lights, &placed));
        if let (Some(a), Some(buffer)) = (ao, &self.ao_uniforms) {
            gpu.queue().write_buffer(buffer, 0, &ao_bytes(view, size, a));
        }

        let [r, g, b, a] = look.background.map(|c| if c.is_finite() { f64::from(c) } else { 0.0 });
        let (result, error) = gpu.scoped(|| -> Result<(), Error> {
            let device = gpu.device();
            let draw_group = self.draws.as_ref().map(|b| {
                gpu.queue().write_buffer(b, 0, &calls.bytes);
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("render draws"),
                    layout: &pipelines.draw_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: b,
                            offset: 0,
                            size: wgpu::BufferSize::new(DRAW_BYTES),
                        }),
                    }],
                })
            });
            let enc = Encoding {
                device,
                pipelines: &pipelines,
                geo,
                calls: &calls,
                wires: &wires,
                shared: &shared,
                groups: passes::texture_groups(device, &pipelines, &calls, &shared.white),
                draw_group,
                frame_group: passes::frame_group(
                    device,
                    &pipelines,
                    &shared.frame,
                    &shared.sampler,
                    "render frame",
                ),
                light_group: self.light_group(device, &pipelines, &shared, lights, &placed, ao.is_some())?,
                background: wgpu::Color { r, g, b, a },
            };
            let mut encoder =
                device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("render") });
            self.encode_shadows(&mut encoder, &enc, &pipes, layers);
            self.encode_ao(&mut encoder, &enc, &pipes);
            self.encode_passes(&mut encoder, &enc, &pipes, color, data, measuring.as_ref());
            if let Some(m) = &measuring {
                m.copy(&mut encoder);
            }
            gpu.submit([encoder.finish()]);
            Ok(())
        });
        if let Some(e) = error {
            return Err(Error::new(format!("render failed: {e}")));
        }
        result?;
        Ok(measuring.map(MeasureTargets::read_back))
    }

    /// The attachments of a render of `size`, made again when the size changes.
    fn ensure_targets(&mut self, gpu: &Gpu, size: (u32, u32), wanted: Wanted) -> Result<(), Error> {
        let samples = wanted.samples;
        let ((), error) = gpu.scoped(|| {
            if self.size != size {
                self.size = size;
                self.msaa_color = None;
                self.depth = None;
                self.depth_data = None;
                self.ao = None;
            }
            if samples > 1 && self.msaa_color.is_none() {
                self.msaa_color = Some(Self::attachment(gpu, size, FORMAT, samples));
            }
            if self.depth.as_ref().is_none_or(|(s, _)| *s != samples) {
                self.depth = Some((samples, Self::attachment(gpu, size, DEPTH, samples)));
            }
            // One depth without multisampling for the data pass and the prepass alike.
            if (wanted.data || wanted.ao) && self.depth_data.is_none() {
                self.depth_data = Some(Self::attachment(gpu, size, DEPTH, 1));
            }
            if wanted.ao && self.ao.is_none() {
                self.ao = Some(AoTargets::new(gpu, size));
            }
            if wanted.ao && self.ao_uniforms.is_none() {
                self.ao_uniforms = Some(gpu.device().create_buffer(&wgpu::BufferDescriptor {
                    label: Some("render ao"),
                    size: AO_BYTES,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
            }
        });
        match error {
            Some(e) => Err(Error::new(format!("cannot create the render targets: {e}"))),
            None => Ok(()),
        }
    }

    /// Room for `need` bytes of draw uniforms.
    fn ensure_draws(&mut self, gpu: &Gpu, need: u64) {
        if need > 0 && self.draws.as_ref().is_none_or(|b| b.size() < need) {
            self.draws = Some(gpu.device().create_buffer(&wgpu::BufferDescriptor {
                label: Some("render draws"),
                size: need.next_power_of_two().max(4096),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
    }

    /// Shadow and tint maps of these sizes with at least `layers` layers, and a frame uniform
    /// buffer per layer.
    fn ensure_maps(
        &mut self,
        gpu: &Gpu,
        shadow_size: u32,
        tint_size: u32,
        layers: u32,
        tinted: bool,
    ) -> Result<(), Error> {
        let ((), error) = gpu.scoped(|| {
            let fits = |m: &Option<ShadowMaps>, size: u32, layers: u32| {
                m.as_ref().is_some_and(|m| m.size == size && m.layers >= layers.max(1))
            };
            if !fits(&self.shadows, shadow_size, layers) {
                self.shadows = Some(ShadowMaps::new(gpu, shadow_size, layers.max(1), DEPTH));
            }
            let tint_layers = if tinted { layers } else { 1 };
            if !fits(&self.tints, tint_size, tint_layers) {
                self.tints = Some(ShadowMaps::new(gpu, tint_size, tint_layers.max(1), FORMAT));
            }
            while (self.shadow_frames.len() as u32) < layers {
                self.shadow_frames.push(gpu.device().create_buffer(&wgpu::BufferDescriptor {
                    label: Some("shadow frame"),
                    size: FRAME_BYTES,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }));
            }
        });
        match error {
            Some(e) => Err(Error::new(format!("cannot create the shadow maps: {e}"))),
            None => Ok(()),
        }
    }

    /// The samplers, the white texture and the frame buffer, made on first use.
    fn shared(&mut self, gpu: &Gpu) -> Result<Shared, Error> {
        let (compare, clamp, around) = self
            .light_samplers
            .get_or_insert_with(|| {
                let compare = gpu.device().create_sampler(&wgpu::SamplerDescriptor {
                    label: Some("shadow maps"),
                    mag_filter: wgpu::FilterMode::Linear,
                    min_filter: wgpu::FilterMode::Linear,
                    compare: Some(wgpu::CompareFunction::LessEqual),
                    ..Default::default()
                });
                let clamp = gpu.device().create_sampler(&wgpu::SamplerDescriptor {
                    label: Some("projector pictures"),
                    mag_filter: wgpu::FilterMode::Linear,
                    min_filter: wgpu::FilterMode::Linear,
                    ..Default::default()
                });
                (compare, clamp, crate::environment::sampler(gpu, "environment"))
            })
            .clone();
        let white = match &self.white {
            Some(w) => w.clone(),
            None => {
                let d = crate::image::Decoded {
                    width: 1,
                    height: 1,
                    rgba: vec![half::f16::ONE; 4],
                    source: String::new(),
                };
                let w = gpu.upload(&d, "white")?;
                self.white = Some(w.clone());
                w
            }
        };
        let sampler = self
            .sampler
            .get_or_insert_with(|| {
                gpu.device().create_sampler(&wgpu::SamplerDescriptor {
                    label: Some("render"),
                    address_mode_u: wgpu::AddressMode::Repeat,
                    address_mode_v: wgpu::AddressMode::Repeat,
                    mag_filter: wgpu::FilterMode::Linear,
                    min_filter: wgpu::FilterMode::Linear,
                    ..Default::default()
                })
            })
            .clone();
        let frame = self
            .frame
            .get_or_insert_with(|| {
                gpu.device().create_buffer(&wgpu::BufferDescriptor {
                    label: Some("render frame"),
                    size: FRAME_BYTES,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            })
            .clone();
        Ok(Shared { compare, clamp, around, sampler, white, frame })
    }
}

/// Ambient occlusion, only where it can show: on lit surfaces, with ambient or environment light
/// to dim.
fn ambient_occlusion(look: &Look, lights: &[Light], placed: &[Placed]) -> Option<Ao> {
    let ambient = lights
        .iter()
        .zip(placed)
        .any(|(l, p)| !p.dropped && matches!(l.kind, LightKind::Ambient | LightKind::Environment));
    look.ao.filter(|a| {
        ambient
            && !look.shade_all
            && matches!(look.surface, Surface::Material | Surface::Texture)
            && a.radius.is_finite()
            && a.radius > 0.0
            && a.strength.is_finite()
            && a.strength > 0.0
    })
}

/// The sizes of the shadow and tint maps, whether the tint maps hold anything, and whether
/// ambient occlusion was drawn.
#[derive(Clone, Copy, Debug)]
struct Maps {
    shadow: u32,
    tint: u32,
    tinted: bool,
    ao: bool,
}

/// Column-major `f32`s of a row-major matrix (WGSL's layout).
fn columns(m: &Mat4) -> impl Iterator<Item = f32> + '_ {
    let t = crate::geo::transpose(m);
    t.into_iter().flatten().map(|x| x as f32)
}

/// render.wgsl's `Frame`; `placed` beside `lights`.
fn frame_bytes(view: &View, look: &Look, maps: Maps, lights: &[Light], placed: &[Placed]) -> Vec<u8> {
    let mut b = Vec::with_capacity(FRAME_BYTES as usize);
    let mut f = |v: f32| b.extend_from_slice(&(if v.is_finite() { v } else { 0.0 }).to_le_bytes());
    for v in columns(&view.clip_from_world) {
        f(v);
    }
    for v in view.view_z {
        f(v as f32);
    }
    // w 1: the eye's position; w 0: the direction to the viewer of a parallel view.
    if view.parallel {
        for v in &view.view_z[..3] {
            f(-*v as f32);
        }
        f(0.0);
    } else {
        for v in view.eye {
            f(v as f32);
        }
        f(1.0);
    }
    let mode: u32 = match look.surface {
        Surface::Material => 0,
        Surface::Texture => 1,
        Surface::Parts => 2,
        Surface::Uv => 3,
        Surface::Normals => 4,
    };
    let overlay = look.overlay.filter(|_| look.surface == Surface::Texture);
    let lights = lights.get(..MAX_LIGHTS).unwrap_or(lights);
    let lights = lights.get(..placed.len()).unwrap_or(lights);
    for v in [mode, lights.len() as u32, u32::from(look.shade_all), u32::from(overlay.is_some())] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    // The overlay as projection.wgsl takes it (gpu::projection::Overlay).
    let o = overlay.unwrap_or(Overlay { factor: 1.0, scale: [1.0; 2], cap: f32::MAX });
    for v in [o.scale[0], o.scale[1], o.factor, o.cap] {
        let v = if v.is_nan() { 0.0 } else { v.clamp(-f32::MAX, f32::MAX) };
        b.extend_from_slice(&v.to_le_bytes());
    }
    let flag = |on: bool| if on { 1.0 } else { 0.0 };
    for v in [maps.shadow as f32, maps.tint as f32, flag(maps.tinted), flag(maps.ao)] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    for (l, p) in lights.iter().zip(placed) {
        b.extend(light_bytes(l, p));
    }
    b.resize(FRAME_BYTES as usize, 0);
    b
}

/// ao.wgsl's `Ao`: world to clip, the view's depth row, the picture's size, radius and strength.
fn ao_bytes(view: &View, size: (u32, u32), ao: Ao) -> Vec<u8> {
    let mut b = Vec::with_capacity(AO_BYTES as usize);
    let mut f = |v: f32| b.extend_from_slice(&(if v.is_finite() { v } else { 0.0 }).to_le_bytes());
    for v in columns(&view.clip_from_world) {
        f(v);
    }
    for v in view.view_z {
        f(v as f32);
    }
    f(size.0 as f32);
    f(size.1 as f32);
    f(ao.radius);
    f(ao.strength);
    b.resize(AO_BYTES as usize, 0);
    b
}

/// render.wgsl's `Light`: position and kind, direction (an environment light: its turn in radians
/// and its last mip) and the cone's outer cosine, colour and the inner cosine, then whether it
/// fades with distance, its shadow layer and picture slot (each plus one, 0 for none) and its bias,
/// then the matrix to its clip space.
fn light_bytes(l: &Light, p: &Placed) -> Vec<u8> {
    let mut b = Vec::with_capacity(LIGHT_BYTES as usize);
    let mut f = |v: f32| b.extend_from_slice(&(if v.is_finite() { v } else { 0.0 }).to_le_bytes());
    let kind = match l.kind {
        LightKind::Ambient => 0.0,
        LightKind::Directional => 1.0,
        LightKind::Point => 2.0,
        LightKind::Spot => 3.0,
        LightKind::Projector => 4.0,
        LightKind::Environment => 5.0,
    };
    let [x, y, z] = l.direction;
    let len = (x * x + y * y + z * z).sqrt();
    let dir = match &l.environment {
        // An environment light: its turn in radians and its last mip instead.
        Some(e) => [e.turn.to_radians(), f64::from(e.maps.levels.saturating_sub(1)), 0.0],
        None if len > 1e-12 && len.is_finite() => [x / len, y / len, z / len],
        None => [0.0, 0.0, -1.0],
    };
    for v in l.position {
        f(v as f32);
    }
    f(kind);
    for v in dir {
        f(v as f32);
    }
    f(l.cone[0].clamp(-1.0, 1.0));
    for v in l.color {
        f(if p.dropped { 0.0 } else { v.max(0.0) });
    }
    f(l.cone[1].clamp(-1.0, 1.0));
    f(if l.inverse_square { 1.0 } else { 0.0 });
    f(p.layer.map_or(0.0, |k| (k + 1) as f32));
    f(p.slot.map_or(0.0, |k| (k + 1) as f32));
    f(l.bias.clamp(0.0, 100.0));
    for v in columns(&p.clip) {
        f(v);
    }
    b.resize(LIGHT_BYTES as usize, 0);
    b
}

/// render.wgsl's `Draw`; `occluder` where `m` hides what is behind it in this render.
fn draw_slot(
    world: &Mat4,
    normals: &[[f64; 3]; 3],
    color: [f32; 4],
    m: &Material,
    occluder: bool,
    part: u32,
    instances: Option<&Instances>,
) -> Vec<u8> {
    let mut b = Vec::with_capacity(DRAW_BYTES as usize);
    let mut f = |v: f32| b.extend_from_slice(&(if v.is_finite() { v } else { 0.0 }).to_le_bytes());
    for v in columns(world) {
        f(v);
    }
    // The normal matrix's columns as three vec4.
    let [[a, bb, c], [d, e, g], [h, i, j]] = *normals;
    for col in [[a, d, h], [bb, e, i], [c, g, j]] {
        for v in col {
            f(v as f32);
        }
        f(0.0);
    }
    for v in color {
        f(v);
    }
    for v in m.specular {
        f(v.max(0.0));
    }
    f(m.shininess.clamp(1.0, 4096.0));
    for v in m.emit {
        f(v.max(0.0));
    }
    f(0.0);
    // An occluder in a picture of a geo node shades as any surface.
    let shading: u32 = match m.shading {
        Shading::Constant => 0,
        Shading::Phong => 1,
        Shading::Pbr => 2,
        Shading::Occluder if occluder => 3,
        Shading::Occluder => 0,
    };
    b.extend_from_slice(&part.to_le_bytes());
    b.extend_from_slice(&shading.to_le_bytes());
    for v in [m.metallic.clamp(0.0, 1.0), m.roughness.clamp(0.0, 1.0)] {
        b.extend_from_slice(&(if v.is_finite() { v } else { 0.0 }).to_le_bytes());
    }
    // The wireframe's width in pixels (0: the whole surface), whether it leaves out the edges
    // inside flat polygons, what its faces show; then their colour.
    let (width, polygons, faces, fill) =
        m.wire.map_or((0.0, false, Faces::Open, [0.0; 4]), |w| (w.width, w.polygons, w.faces, w.fill));
    let width = if width.is_finite() { width.clamp(0.0, 256.0) } else { 1.0 };
    let faces = match faces {
        Faces::Open => 0.0,
        Faces::Filled => 1.0,
        Faces::Hidden => 2.0,
    };
    for v in [width, if polygons { 1.0 } else { 0.0 }, faces, 0.0] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    for v in fill {
        let v = if v.is_finite() { v.clamp(0.0, 1e4) } else { 0.0 };
        b.extend_from_slice(&v.to_le_bytes());
    }
    // Drawn per instance, and which of their textures are there.
    let has = instances.map_or(0u32, |i| {
        u32::from(i.aim.is_some()) | u32::from(i.scale.is_some()) << 1 | u32::from(i.color.is_some()) << 2
    });
    for v in [u32::from(instances.is_some()), has, 0, 0] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b.resize(DRAW_BYTES as usize, 0);
    b
}
