//! The parts of one render: its draws and the orders the passes take them in, the pipelines it
//! uses, the measuring pass's targets, and the encoding of the shadow, ambient occlusion and main
//! passes.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use penumbra_core::Error;

use super::mips::Mipped;
use super::{
    DEPTH, Light, Look, MAX_DRAWS, MAX_PROJECTORS, MEASURE, Measurement, OPAQUE, Pass, Pipelines, Pipes,
    Placed, Renderer, Stage, Surface, View, draw_slot, measure_size,
};
use crate::geo::{Faces, Geo, Instances, Material, Shading, normal_matrix};
use crate::{Gpu, Tex};

/// One run of a part and material, with its uniforms.
pub(super) struct DrawCall {
    model: usize,
    first: u32,
    count: u32,
    slot: u32,
    texture: Option<Tex>,
    /// Drawn once per instance; `None`: once.
    instances: Option<Arc<Instances>>,
    /// Can show see-through pixels: drawn again, sorted, after the opaque ones.
    clear: bool,
    /// Drawn as the edges of its triangles, from the model's wireframe vertices.
    wire: bool,
    /// Shows nothing but hides what is behind it: drawn before everything else.
    occluder: bool,
    /// A wireframe with hidden faces: drawn after everything else in the colour, data and
    /// measuring passes, its faces' depth first, then its lines where that is the nearest.
    hides: bool,
    /// Drawn in the shadow and tint passes.
    shadows: bool,
    /// Distance of its middle along the view, for the sorting.
    depth: f64,
}

/// A render's draws, their uniforms, and the orders the passes draw them in (indices into
/// `calls`).
pub(super) struct Calls {
    calls: Vec<DrawCall>,
    /// The draws' uniforms, one slot of `align` bytes each.
    pub(super) bytes: Vec<u8>,
    align: u64,
    /// Everything but the wireframes with hidden faces, occluders first: they leave the colour as
    /// it is, so what was drawn before them would show through.
    rest: Vec<usize>,
    /// Wireframes with hidden faces, which the colour, data and measuring passes draw last.
    hiding: Vec<usize>,
    /// See-through draws, farthest first.
    behind: Vec<usize>,
    /// What casts shadows: whole, and the see-through draws among it tinting the light.
    casting: Vec<usize>,
    pub(super) tinting: Vec<usize>,
}

impl Calls {
    /// One draw per run of a part and material of `geo`, seen through `view`; `texture` is the
    /// input texture for [`Surface::Texture`]. Uniform slots are `align` bytes apart.
    pub(super) fn new(
        geo: &Geo,
        view: &View,
        look: &Look,
        texture: Option<&Tex>,
        align: u64,
    ) -> Result<Self, Error> {
        // Materials show only where the surface is theirs; parts, uv and normals are whole.
        let shows_material = matches!(look.surface, Surface::Material | Surface::Texture);
        let mut calls = Vec::new();
        let mut bytes: Vec<u8> = Vec::new();
        for (index, item) in geo.items.iter().enumerate() {
            let model = &item.model;
            let normals = normal_matrix(&item.world);
            for (k, r) in model.mesh.ranges.iter().enumerate() {
                if calls.len() >= MAX_DRAWS {
                    return Err(Error::new(format!("more than {MAX_DRAWS} draws")));
                }
                let file = model.mesh.materials.get(r.material as usize);
                let plain = Material::constant();
                // The geometry's material where the surface shows one; parts, uv and normals
                // are flat.
                let m = match item.material.as_deref() {
                    Some(m) if shows_material => m,
                    _ => &plain,
                };
                let wire = m.wire.is_some();
                let occluder = m.shading == Shading::Occluder && !look.shade_all;
                let hides = m.wire.is_some_and(|w| w.faces == Faces::Hidden);
                // Filled faces are see-through where their colour is.
                let fill_clear = m.wire.is_some_and(|w| w.faces == Faces::Open || w.fill[3] < OPAQUE);
                // A geometry's material replaces the file's colour and textures; without one
                // the file's show as they are.
                let tex = match (look.surface, item.material.as_deref()) {
                    (Surface::Material, Some(m)) => m.texture.clone(),
                    (Surface::Material, None) => file
                        .and_then(|mat| mat.texture)
                        .and_then(|t| model.textures.get(t as usize).cloned().flatten()),
                    (Surface::Texture, _) => texture.cloned(),
                    _ => None,
                };
                let color = match (look.surface, item.material.as_deref()) {
                    (Surface::Material, None) => file.map_or([1.0; 4], |mat| mat.color),
                    _ => m.color,
                };
                // A texture may hold see-through texels anywhere; a colour says so itself. A
                // wireframe is mostly see-through, an occluder never.
                let clear = !occluder
                    && !hides
                    && (fill_clear
                        || match look.surface {
                            Surface::Material => color[3] < OPAQUE || tex.is_some(),
                            Surface::Texture => tex.is_some(),
                            _ => false,
                        });
                let center = model.centers.get(k).copied().unwrap_or_default().map(f64::from);
                let [cx, cy, cz] = crate::geo::transform(&item.world, center);
                let z = view.view_z;
                let depth = z[0] * cx + z[1] * cy + z[2] * cz + z[3];
                let slot = calls.len() as u32;
                bytes.resize(slot as usize * align as usize, 0);
                bytes.extend(draw_slot(
                    &item.world,
                    &normals,
                    color,
                    m,
                    occluder,
                    item.part_offset + r.part,
                    item.instances.as_deref(),
                ));
                calls.push(DrawCall {
                    model: index,
                    first: r.first,
                    count: r.count,
                    slot,
                    texture: tex,
                    instances: item.instances.clone(),
                    clear,
                    wire,
                    occluder,
                    hides,
                    shadows: m.shadows,
                    depth: if depth.is_finite() { depth } else { 0.0 },
                });
            }
        }
        let pick = |f: fn(&DrawCall) -> bool| -> Vec<usize> {
            (0..calls.len()).filter(|k| calls.get(*k).is_some_and(f)).collect()
        };
        let mut order: Vec<usize> = (0..calls.len()).collect();
        order.sort_by_key(|k| calls.get(*k).is_none_or(|c| !c.occluder));
        let hiding = pick(|c| c.hides);
        let rest = order.into_iter().filter(|k| !hiding.contains(k)).collect();
        let mut behind = pick(|c| c.clear);
        behind.sort_by(|a, b| {
            let depth = |k: &usize| calls.get(*k).map_or(0.0, |c| c.depth);
            depth(b).total_cmp(&depth(a))
        });
        let casting = pick(|c| c.shadows);
        let tinting = behind.iter().copied().filter(|k| calls.get(*k).is_some_and(|c| c.shadows)).collect();
        Ok(Self { calls, bytes, align, rest, hiding, behind, casting, tinting })
    }

    /// The textures the draws sample, a texture as often as draws name it.
    pub(super) fn textures(&self) -> impl Iterator<Item = &Tex> {
        self.calls.iter().filter_map(|c| c.texture.as_ref())
    }

    /// The wireframe vertices of every model drawn as one.
    pub(super) fn wires(&self, gpu: &Gpu, geo: &Geo) -> Result<HashMap<usize, wgpu::Buffer>, Error> {
        let mut wires = HashMap::new();
        for c in self.calls.iter().filter(|c| c.wire) {
            if let (false, Some(item)) = (wires.contains_key(&c.model), geo.items.get(c.model)) {
                wires.insert(c.model, item.model.wire(gpu)?);
            }
        }
        Ok(wires)
    }
}

/// Which passes a render draws besides the colour pass, and the colour pass's samples.
#[derive(Clone, Copy, Debug)]
pub(super) struct Wanted {
    pub(super) samples: u32,
    pub(super) data: bool,
    pub(super) measure: bool,
    /// A light casts shadows.
    pub(super) shadows: bool,
    /// ... and something see-through tints them.
    pub(super) tinted: bool,
    pub(super) ao: bool,
}

/// The pipelines one render uses; `None` for the passes it leaves out.
pub(super) struct RenderPipes {
    color: Pipes,
    data: Option<Pipes>,
    measure: Option<Pipes>,
    shadow: Option<Pipes>,
    tint: Option<Pipes>,
    prepass: Option<Pipes>,
    /// For the see-through draws: one with culling; without, each draw twice, its back faces
    /// then its front faces (a closed glass body shows its far side through its near one).
    blend: Vec<Pipes>,
    /// Hidden faces' two steps, per pass that draws them; only wireframes use them.
    solid_stages: Option<(Pipes, Pipes)>,
    data_stages: Option<(Pipes, Pipes)>,
    measure_stages: Option<(Pipes, Pipes)>,
}

impl RenderPipes {
    pub(super) fn new(
        gpu: &Gpu,
        pipelines: &Pipelines,
        look: &Look,
        calls: &Calls,
        wanted: Wanted,
    ) -> Result<Self, Error> {
        let cull = look.cull.then_some(wgpu::Face::Back);
        let any_wire = calls.calls.iter().any(|c| c.wire);
        let pipes = |samples: u32, pass: Pass, cull: Option<wgpu::Face>| -> Result<Pipes, Error> {
            Ok(Pipes {
                plain: pipelines.pipeline(gpu, samples, pass, cull, false, Stage::Draw)?,
                wire: if any_wire {
                    Some(pipelines.pipeline(gpu, samples, pass, cull, true, Stage::Draw)?)
                } else {
                    None
                },
            })
        };
        let some =
            |on: bool, samples: u32, pass: Pass, cull: Option<wgpu::Face>| -> Result<Option<Pipes>, Error> {
                if on { pipes(samples, pass, cull).map(Some) } else { Ok(None) }
            };
        let stages = |on: bool, samples: u32, pass: Pass| -> Result<Option<(Pipes, Pipes)>, Error> {
            if !on || calls.hiding.is_empty() {
                return Ok(None);
            }
            let wire = |stage| -> Result<Pipes, Error> {
                let p = pipelines.pipeline(gpu, samples, pass, cull, true, stage)?;
                Ok(Pipes { plain: p.clone(), wire: Some(p) })
            };
            Ok(Some((wire(Stage::Hide)?, wire(Stage::Over)?)))
        };
        let blend = if calls.behind.is_empty() {
            Vec::new()
        } else if look.cull {
            vec![pipes(wanted.samples, Pass::Blend, cull)?]
        } else {
            vec![
                pipes(wanted.samples, Pass::Blend, Some(wgpu::Face::Front))?,
                pipes(wanted.samples, Pass::Blend, Some(wgpu::Face::Back))?,
            ]
        };
        Ok(Self {
            color: pipes(wanted.samples, Pass::Solid, cull)?,
            data: some(wanted.data, 1, Pass::Data, cull)?,
            measure: some(wanted.measure, 1, Pass::Measure, cull)?,
            // Both faces cast shadows, whatever the colour pass culls.
            shadow: some(wanted.shadows, 1, Pass::Shadow, None)?,
            tint: some(wanted.tinted, 1, Pass::Tint, None)?,
            prepass: some(wanted.ao, 1, Pass::Prepass, cull)?,
            blend,
            solid_stages: stages(true, wanted.samples, Pass::Solid)?,
            data_stages: stages(wanted.data, 1, Pass::Data)?,
            measure_stages: stages(wanted.measure, 1, Pass::Measure)?,
        })
    }
}

/// The measuring pass's targets and the buffer it is read back through: made anew each time
/// (measurements are rare, at most one per node in flight).
pub(super) struct MeasureTargets {
    size: (u32, u32),
    /// Bytes per row in the buffer.
    padded: u64,
    target: wgpu::Texture,
    view: wgpu::TextureView,
    depth: wgpu::TextureView,
    buffer: wgpu::Buffer,
}

impl MeasureTargets {
    /// Targets for measuring a render of `size`, at [`measure_size`].
    pub(super) fn new(gpu: &Gpu, size: (u32, u32)) -> Result<Self, Error> {
        let msize = measure_size(size);
        let padded = (u64::from(msize.0) * 16).div_ceil(256) * 256;
        let (made, error) = gpu.scoped(|| {
            let target = gpu.device().create_texture(&wgpu::TextureDescriptor {
                label: Some("render measure"),
                size: wgpu::Extent3d { width: msize.0, height: msize.1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: MEASURE,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = target.create_view(&wgpu::TextureViewDescriptor::default());
            let depth = Renderer::attachment(gpu, msize, DEPTH, 1);
            let buffer = gpu.device().create_buffer(&wgpu::BufferDescriptor {
                label: Some("render measure"),
                size: padded * u64::from(msize.1),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            Self { size: msize, padded, target, view, depth, buffer }
        });
        match error {
            Some(e) => Err(Error::new(format!("cannot create the measuring targets: {e}"))),
            None => Ok(made),
        }
    }

    /// Copies the measuring pass's result into the buffer.
    pub(super) fn copy(&self, encoder: &mut wgpu::CommandEncoder) {
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(u32::try_from(self.padded).unwrap_or(u32::MAX)),
                    rows_per_image: Some(self.size.1),
                },
            },
            wgpu::Extent3d { width: self.size.0, height: self.size.1, depth_or_array_layers: 1 },
        );
    }

    /// Starts reading the buffer back, after the submission that copied into it.
    pub(super) fn read_back(self) -> Measurement {
        let state = Arc::new(AtomicU8::new(0));
        let done = state.clone();
        self.buffer.map_async(wgpu::MapMode::Read, .., move |r| {
            done.store(if r.is_ok() { 1 } else { 2 }, Ordering::Release);
        });
        Measurement { buffer: self.buffer, size: self.size, padded: self.padded, state }
    }
}

/// What a renderer makes once and keeps: the samplers, a white texture for free slots, and the
/// frame's uniform buffer.
#[derive(Clone)]
pub(super) struct Shared {
    /// The shadow maps'.
    pub(super) compare: wgpu::Sampler,
    /// The projector pictures', clamped at the edges.
    pub(super) clamp: wgpu::Sampler,
    /// The environment's, around in u.
    pub(super) around: wgpu::Sampler,
    /// The surfaces' textures'.
    pub(super) sampler: wgpu::Sampler,
    pub(super) white: Tex,
    pub(super) frame: wgpu::Buffer,
}

/// Everything the passes of one render draw with.
pub(super) struct Encoding<'a> {
    pub(super) device: &'a wgpu::Device,
    pub(super) pipelines: &'a Pipelines,
    pub(super) geo: &'a Geo,
    pub(super) calls: &'a Calls,
    pub(super) wires: &'a HashMap<usize, wgpu::Buffer>,
    pub(super) shared: &'a Shared,
    /// Each draw's texture group.
    pub(super) groups: Vec<wgpu::BindGroup>,
    /// The draws' uniforms; `None` without any draw.
    pub(super) draw_group: Option<wgpu::BindGroup>,
    pub(super) frame_group: wgpu::BindGroup,
    /// Shadow maps, projector pictures, the environment and ambient occlusion.
    pub(super) light_group: wgpu::BindGroup,
    /// The colour pass's background.
    pub(super) background: wgpu::Color,
}

pub(super) fn view_entry(binding: u32, v: &wgpu::TextureView) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry { binding, resource: wgpu::BindingResource::TextureView(v) }
}

/// A group of a uniform buffer and a sampler, as the frame layout wants it.
pub(super) fn frame_group(
    device: &wgpu::Device,
    pipelines: &Pipelines,
    buffer: &wgpu::Buffer,
    sampler: &wgpu::Sampler,
    label: &str,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout: &pipelines.frame_layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) },
        ],
    })
}

/// One texture group per draw (white without a texture, white for missing instance
/// textures), made once per distinct texture and instances; the texture through its mip chain.
pub(super) fn texture_groups(
    device: &wgpu::Device,
    pipelines: &Pipelines,
    calls: &Calls,
    white: &Tex,
    mipped: &Mipped,
) -> Vec<wgpu::BindGroup> {
    type Key = (Tex, Option<Arc<Instances>>);
    let same = |a: &Key, b: &Key| {
        a.0.same(&b.0)
            && match (&a.1, &b.1) {
                (Some(x), Some(y)) => Arc::ptr_eq(x, y),
                (None, None) => true,
                _ => false,
            }
    };
    let mut made: Vec<(Key, wgpu::BindGroup)> = Vec::new();
    let mut group_of = |key: Key| -> wgpu::BindGroup {
        if let Some((_, g)) = made.iter().find(|(x, _)| same(x, &key)) {
            return g.clone();
        }
        let inst = key.1.as_deref();
        fn or<'a>(t: Option<&'a Tex>, white: &'a Tex) -> &'a wgpu::TextureView {
            t.unwrap_or(white).view()
        }
        let g = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("render texture"),
            layout: &pipelines.tex_layout,
            entries: &[
                view_entry(0, mipped.view(&key.0)),
                view_entry(1, or(inst.map(|i| &i.position), white)),
                view_entry(2, or(inst.and_then(|i| i.aim.as_ref()), white)),
                view_entry(3, or(inst.and_then(|i| i.scale.as_ref()), white)),
                view_entry(4, or(inst.and_then(|i| i.color.as_ref()), white)),
            ],
        });
        made.push((key, g.clone()));
        g
    };
    calls
        .calls
        .iter()
        .map(|c| group_of((c.texture.clone().unwrap_or_else(|| white.clone()), c.instances.clone())))
        .collect()
}

impl Encoding<'_> {
    /// Draws `calls[k]` for each k in `which` with `pipes`, binding a model's buffers and
    /// switching between surfaces and wireframes only when that changes.
    fn draw(&self, rp: &mut wgpu::RenderPass<'_>, pipes: &Pipes, which: impl IntoIterator<Item = usize>) {
        let Some(draw_group) = &self.draw_group else { return };
        let mut bound: Option<(usize, bool)> = None;
        let mut set: Option<bool> = None;
        for k in which {
            let (Some(call), Some(tg)) = (self.calls.calls.get(k), self.groups.get(k)) else { continue };
            let Some(item) = self.geo.items.get(call.model) else { continue };
            if set != Some(call.wire) {
                let pipeline = if call.wire { pipes.wire.as_ref() } else { Some(&pipes.plain) };
                let Some(pipeline) = pipeline else { continue };
                rp.set_pipeline(pipeline);
                set = Some(call.wire);
            }
            if bound != Some((call.model, call.wire)) {
                if call.wire {
                    let Some(b) = self.wires.get(&call.model) else { continue };
                    rp.set_vertex_buffer(0, b.slice(..));
                } else {
                    rp.set_vertex_buffer(0, item.model.vertices.slice(..));
                    rp.set_index_buffer(item.model.indices.slice(..), wgpu::IndexFormat::Uint32);
                }
                bound = Some((call.model, call.wire));
            }
            rp.set_bind_group(1, draw_group, &[(u64::from(call.slot) * self.calls.align) as u32]);
            rp.set_bind_group(2, tg, &[]);
            let copies = call.instances.as_ref().map_or(1, |i| i.count());
            if call.wire {
                // The wireframe's corners lie in index order, three per triangle.
                rp.draw(call.first..call.first + call.count, 0..copies);
            } else {
                rp.draw_indexed(call.first..call.first + call.count, 0, 0..copies);
            }
        }
    }
}

/// A colour attachment cleared to transparent and stored.
fn cleared(view: &wgpu::TextureView) -> Option<wgpu::RenderPassColorAttachment<'_>> {
    Some(wgpu::RenderPassColorAttachment {
        view,
        depth_slice: None,
        resolve_target: None,
        ops: wgpu::Operations {
            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
            store: wgpu::StoreOp::Store,
        },
    })
}

/// A depth attachment cleared to the far plane.
fn depth_cleared(
    view: &wgpu::TextureView,
    store: wgpu::StoreOp,
) -> Option<wgpu::RenderPassDepthStencilAttachment<'_>> {
    Some(wgpu::RenderPassDepthStencilAttachment {
        view,
        depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store }),
        stencil_ops: None,
    })
}

impl Renderer {
    /// The light group: the shadow and tint maps, the projector pictures (through their mip
    /// chains) and the first environment light's maps (white where a slot is free), and ambient
    /// occlusion's result (white without it; the shader then does not read it).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn light_group(
        &self,
        device: &wgpu::Device,
        pipelines: &Pipelines,
        shared: &Shared,
        lights: &[Light],
        placed: &[Placed],
        ao: bool,
        mipped: &Mipped,
    ) -> Result<wgpu::BindGroup, Error> {
        let (Some(shadows), Some(tints)) = (self.shadows.as_ref(), self.tints.as_ref()) else {
            return Err(Error::new("no shadow maps"));
        };
        let white = shared.white.view();
        let mut pictures = [white; MAX_PROJECTORS];
        for (p, l) in placed.iter().zip(lights) {
            let picture = l.projector.as_ref().and_then(|j| j.texture.as_ref());
            if let (Some(slot), Some(t)) = (p.slot.and_then(|k| pictures.get_mut(k as usize)), picture) {
                *slot = mipped.view(t);
            }
        }
        let [p0, p1, p2, p3] = pictures;
        let environment = placed
            .iter()
            .zip(lights)
            .find(|(p, l)| !p.dropped && l.environment.is_some())
            .and_then(|(_, l)| l.environment.as_ref());
        let (specular, irradiance) = match environment {
            Some(e) => (&e.maps.specular, &e.maps.irradiance),
            None => (white, white),
        };
        let occlusion = match (&self.ao, ao) {
            (Some(t), true) => &t.blurred,
            _ => white,
        };
        let sampler =
            |binding, s| wgpu::BindGroupEntry { binding, resource: wgpu::BindingResource::Sampler(s) };
        Ok(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("render lights"),
            layout: &pipelines.light_layout,
            entries: &[
                view_entry(0, &shadows.array),
                sampler(1, &shared.compare),
                view_entry(2, p0),
                view_entry(3, p1),
                view_entry(4, p2),
                view_entry(5, p3),
                sampler(6, &shared.clamp),
                view_entry(7, &tints.array),
                view_entry(8, specular),
                view_entry(9, irradiance),
                sampler(10, &shared.around),
                view_entry(11, occlusion),
            ],
        }))
    }

    /// The shadow maps, one layer per light casting shadows (the first `layers`); then, where
    /// something is see-through, what it lets through.
    pub(super) fn encode_shadows(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        enc: &Encoding<'_>,
        pipes: &RenderPipes,
        layers: u32,
    ) {
        let (Some(pipeline), Some(_), Some(shadows), Some(tints)) =
            (&pipes.shadow, &enc.draw_group, &self.shadows, &self.tints)
        else {
            return;
        };
        for (k, buffer) in self.shadow_frames.iter().take(layers as usize).enumerate() {
            let from = frame_group(enc.device, enc.pipelines, buffer, &enc.shared.sampler, "shadow frame");
            if let Some(layer) = shadows.layer_views.get(k) {
                let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("shadow"),
                    color_attachments: &[],
                    depth_stencil_attachment: depth_cleared(layer, wgpu::StoreOp::Store),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                rp.set_pipeline(&pipeline.plain);
                rp.set_bind_group(0, &from, &[]);
                enc.draw(&mut rp, pipeline, enc.calls.casting.iter().copied());
            }
            if let (Some(tint), Some(layer)) = (&pipes.tint, tints.layer_views.get(k)) {
                // Everything passes, nothing is near (half float's largest).
                let open = wgpu::Color { r: 1.0, g: 1.0, b: 1.0, a: 65504.0 };
                let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("tint"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: layer,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(open),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                rp.set_pipeline(&tint.plain);
                rp.set_bind_group(0, &from, &[]);
                enc.draw(&mut rp, tint, enc.calls.tinting.iter().copied());
            }
        }
    }

    /// Ambient occlusion: the opaque surfaces' positions and normals, the estimate from them,
    /// and its blur.
    pub(super) fn encode_ao(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        enc: &Encoding<'_>,
        pipes: &RenderPipes,
    ) {
        let (Some(prepass), Some(t), Some(depth), Some(uniforms)) =
            (&pipes.prepass, &self.ao, &self.depth_data, &self.ao_uniforms)
        else {
            return;
        };
        {
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ao prepass"),
                color_attachments: &[cleared(&t.positions), cleared(&t.normals)],
                depth_stencil_attachment: depth_cleared(depth, wgpu::StoreOp::Discard),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if enc.draw_group.is_some() {
                rp.set_pipeline(&prepass.plain);
                rp.set_bind_group(0, &enc.frame_group, &[]);
                enc.draw(&mut rp, prepass, 0..enc.calls.calls.len());
            }
        }
        // The estimate reads positions and normals (its fourth binding is unused), the blur also
        // the estimate.
        let pipelines = enc.pipelines;
        for (pipeline, target, read) in
            [(&pipelines.ao_estimate, &t.estimate, &t.normals), (&pipelines.ao_blur, &t.blurred, &t.estimate)]
        {
            let group = enc.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("render ao"),
                layout: &pipelines.ao_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: uniforms.as_entire_binding() },
                    view_entry(1, &t.positions),
                    view_entry(2, &t.normals),
                    view_entry(3, read),
                ],
            });
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ao"),
                color_attachments: &[cleared(target)],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            rp.set_pipeline(pipeline);
            rp.set_bind_group(0, &group, &[]);
            rp.draw(0..3, 0..1);
        }
    }

    /// The colour pass into `color`, the data pass into `data` and the measuring pass, each:
    /// the opaque draws, in the colour pass the see-through ones over them, then the
    /// wireframes with hidden faces.
    pub(super) fn encode_passes(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        enc: &Encoding<'_>,
        pipes: &RenderPipes,
        color: &Tex,
        data: Option<&Tex>,
        measuring: Option<&MeasureTargets>,
    ) {
        /// A pass: its kind, target, resolve target, depth, pipelines and hidden faces' stages.
        type Target<'a> = (
            Pass,
            &'a wgpu::TextureView,
            Option<&'a wgpu::TextureView>,
            &'a wgpu::TextureView,
            &'a Pipes,
            Option<&'a (Pipes, Pipes)>,
        );
        let mut passes: Vec<Target<'_>> = Vec::new();
        if let Some((samples, depth)) = &self.depth {
            let solid = pipes.solid_stages.as_ref();
            match (&self.msaa_color, samples) {
                (Some(m), 4) => passes.push((Pass::Solid, m, Some(color.view()), depth, &pipes.color, solid)),
                _ => passes.push((Pass::Solid, color.view(), None, depth, &pipes.color, solid)),
            }
        }
        if let (Some(t), Some(p), Some(depth)) = (data, pipes.data.as_ref(), self.depth_data.as_ref()) {
            passes.push((Pass::Data, t.view(), None, depth, p, pipes.data_stages.as_ref()));
        }
        if let (Some(m), Some(p)) = (measuring, pipes.measure.as_ref()) {
            passes.push((Pass::Measure, &m.view, None, &m.depth, p, pipes.measure_stages.as_ref()));
        }
        for (pass, view, resolve, depth_view, pipeline, hidden) in passes {
            let clear = if pass == Pass::Solid { enc.background } else { wgpu::Color::TRANSPARENT };
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("render"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: resolve,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear),
                        store: if resolve.is_some() { wgpu::StoreOp::Discard } else { wgpu::StoreOp::Store },
                    },
                })],
                depth_stencil_attachment: depth_cleared(depth_view, wgpu::StoreOp::Discard),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if enc.draw_group.is_none() {
                continue;
            }
            rp.set_pipeline(&pipeline.plain);
            rp.set_bind_group(0, &enc.frame_group, &[]);
            if pass.colour() {
                rp.set_bind_group(3, &enc.light_group, &[]);
            }
            enc.draw(&mut rp, pipeline, enc.calls.rest.iter().copied());
            // The see-through pixels over the opaque ones, farthest first.
            if pass == Pass::Solid {
                for k in &enc.calls.behind {
                    for p in &pipes.blend {
                        enc.draw(&mut rp, p, [*k]);
                    }
                }
            }
            // Wireframes with hidden faces last: the faces' depth, then the lines where it is
            // the nearest. Their own lines behind fail, and so do other wireframes' behind them;
            // what was drawn before shows through the faces. (Lines behind a see-through surface
            // come out in front of it.)
            if let Some((hide, over)) = hidden {
                enc.draw(&mut rp, hide, enc.calls.hiding.iter().copied());
                enc.draw(&mut rp, over, enc.calls.hiding.iter().copied());
            }
        }
    }
}
