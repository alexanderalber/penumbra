//! `mat` nodes: materials as in TouchDesigner, put on geometry by `geo.assign_material`. `mat.constant`
//! shows its colour as it is, with or without lights; `mat.phong` and `mat.pbr` (metallic and rough
//! as in glTF) answer the render's lights and are black without them. `mat.wireframe` draws only
//! the edges, unlit; `mat.occluder` draws nothing but hides what is behind it.
//!
//! A material replaces the file's colours and textures: its colour, times its texture input
//! where connected. The data on a `mat` connection is a [`Mat`]: the
//! material and a picture of it on a sphere.

use std::sync::Arc;

use half::f16;
use penumbra_core::{Curve, Error, ParamSpec, Unit};
use penumbra_gpu::environment::EnvironmentMaps;
use penumbra_gpu::geo::{Faces, Material, Model, Shading, Wire};
use penumbra_gpu::image::Decoded;
use penumbra_gpu::render::{Environment, Light, LightKind, Renderer};
use penumbra_gpu::{Geo, Gpu, Mat, Tex};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};
use penumbra_mesh::prim;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new("mat.constant", "A colour as it is, unlit: what projection content wants")
            .param(color())
            .input("texture", DataKind::Tex)
            .output("out", DataKind::Mat),
        || Box::new(MatNode::new(Shading::Constant)) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new(
            "mat.pbr",
            "Physically based (metallic, roughness as in glTF): lit by the render's lights, best with an environment light",
        )
        .param(color())
        .param(
            ParamSpec::float("metallic", 0.0)
                .clamp(0.0, 1.0)
                .doc("0: plastic, paint, glass; 1: metal, whose colour tints its reflections"),
        )
        .param(ParamSpec::float("roughness", 0.5).clamp(0.0, 1.0).doc("0: a mirror; 1: matte"))
        .param(emit())
        .input("texture", DataKind::Tex)
        .output("out", DataKind::Mat),
        || Box::new(MatNode::new(Shading::Pbr)) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new("mat.phong", "Lit by the render's lights, with a highlight; black without lights")
            .param(color())
            .param(
                ParamSpec::color("specular", [0.5, 0.5, 0.5, 1.0])
                    .doc("The highlight's colour; alpha is unused"),
            )
            .param(
                ParamSpec::float("shininess", 32.0)
                    .clamp(1.0, 4096.0)
                    .range(1.0, 512.0)
                    .curve(Curve::Log)
                    .doc("Higher: a smaller, sharper highlight"),
            )
            .param(emit())
            .input("texture", DataKind::Tex)
            .output("out", DataKind::Mat),
        || Box::new(MatNode::new(Shading::Phong)) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new("mat.wireframe", "The edges as lines of a width in pixels, the faces open, filled or hidden; unlit")
            .param(
                ParamSpec::color("color", [1.0; 4])
                    .doc("The lines' colour: linear RGBA, straight alpha; times the texture input"),
            )
            .param(
                ParamSpec::float("line_width", 1.5)
                    .clamp(0.1, 64.0)
                    .range(0.5, 8.0)
                    .unit(Unit::Pixels)
                    .doc("The lines' width in pixels of the render"),
            )
            .param(ParamSpec::choice("edges", &["polygons", "triangles"]).doc(
                "polygons: leave out edges inside the file's polygons (OBJ faces), or inside flat faces where it has only triangles (a quad's diagonal); triangles: every edge",
            ))
            .param(ParamSpec::choice("faces", &["open", "filled", "hidden"]).doc(
                "open: nothing, the lines behind show through; filled: the fill colour; hidden: only the lines filled faces would leave, the faces see-through",
            ))
            .param(
                ParamSpec::color("fill", [0.0, 0.0, 0.0, 1.0])
                    .doc("The faces' colour where `faces` is filled: linear RGBA, straight alpha"),
            )
            .param(
                ParamSpec::bool("shadows", true)
                    .doc("Lines and filled faces cast shadows from the lights that cast them; off: none"),
            )
            .input("texture", DataKind::Tex)
            .output("out", DataKind::Mat),
        || Box::new(MatNode::wire()) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new(
            "mat.occluder",
            "Invisible, but hides what is behind it: a real object in front of a projection; casts shadows",
        )
        .output("out", DataKind::Mat),
        || Box::new(MatNode::new(Shading::Occluder)) as Box<dyn Operator>,
    ))?;
    Ok(())
}

fn emit() -> ParamSpec {
    ParamSpec::color("emit", [0.0, 0.0, 0.0, 1.0])
        .more()
        .doc("Light the surface gives off by itself; alpha is unused")
}

fn color() -> ParamSpec {
    ParamSpec::color("color", [1.0; 4])
        .doc("Linear RGBA, straight alpha; replaces the file's colour; times the texture input")
}

fn rgba(cx: &CookContext<'_>, name: &str) -> Result<[f32; 4], CookError> {
    match cx.param(name).and_then(|v| v.components()) {
        Some(&[r, g, b, a]) => Ok([r, g, b, a].map(|c| c as f32)),
        Some(&[r, g, b]) => Ok([r as f32, g as f32, b as f32, 1.0]),
        _ => Err(CookError::failed(format!("`{name}` must be a colour"))),
    }
}

struct MatNode {
    shading: Shading,
    /// `mat.wireframe`.
    wire: bool,
    /// The sphere the picture shows, uploaded once.
    sphere: Option<Arc<Model>>,
    /// Surroundings for a pbr material's picture, baked once: metal shows nothing but its
    /// reflections.
    studio: Option<Arc<EnvironmentMaps>>,
    renderer: Renderer,
}

impl MatNode {
    fn new(shading: Shading) -> Self {
        Self { shading, wire: false, sphere: None, studio: None, renderer: Renderer::new() }
    }

    fn wire() -> Self {
        Self { wire: true, ..Self::new(Shading::Constant) }
    }

    /// A grey studio: bright above, fading to the horizon, a dark floor.
    fn studio(&mut self, gpu: &Gpu) -> Result<Arc<EnvironmentMaps>, Error> {
        if let Some(s) = &self.studio {
            return Ok(s.clone());
        }
        let (w, h) = (64u32, 32u32);
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            // 1 straight up, 0 at the horizon.
            let up = 1.0 - (y as f32 + 0.5) / (h as f32 / 2.0);
            let v = if up > 0.0 { 0.25 + 0.75 * up.powf(0.7) } else { 0.04 };
            for _ in 0..w {
                rgba.extend([f16::from_f32(v), f16::from_f32(v), f16::from_f32(v), f16::ONE]);
            }
        }
        let picture = gpu.upload(&Decoded { width: w, height: h, rgba, source: String::new() }, "studio")?;
        let maps = Arc::new(EnvironmentMaps::bake(gpu, &picture, 128)?);
        self.studio = Some(maps.clone());
        Ok(maps)
    }

    fn sphere(&mut self, gpu: &Gpu) -> Result<Arc<Model>, Error> {
        if let Some(s) = &self.sphere {
            return Ok(s.clone());
        }
        // A wireframe's picture reads better with fewer, larger faces.
        let segments = if self.wire { 16 } else { 48 };
        let (model, _) = Model::upload(gpu, prim::sphere(0.5, segments)?, "material sphere")?;
        let model = Arc::new(model);
        self.sphere = Some(model.clone());
        Ok(model)
    }
}

impl Operator for MatNode {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = cx.resource::<Gpu>().ok_or_else(|| CookError::failed("no GPU: mat nodes cannot cook"))?;
        let mut material = Material { shading: self.shading, ..Material::constant() };
        if self.shading != Shading::Occluder {
            material.color = rgba(cx, "color")?;
        }
        if self.wire {
            let polygons = match cx.text("edges")? {
                "polygons" => true,
                "triangles" => false,
                other => return Err(CookError::failed(format!("unknown edges `{other}`"))),
            };
            let faces = match cx.text("faces")? {
                "open" => Faces::Open,
                "filled" => Faces::Filled,
                "hidden" => Faces::Hidden,
                other => return Err(CookError::failed(format!("unknown faces `{other}`"))),
            };
            let fill = rgba(cx, "fill")?;
            material.wire = Some(Wire { width: cx.float("line_width")? as f32, polygons, faces, fill });
            material.shadows = cx.bool("shadows")?;
        }
        if self.shading == Shading::Phong {
            let [r, g, b, _] = rgba(cx, "specular")?;
            material.specular = [r, g, b];
            material.shininess = cx.float("shininess")? as f32;
            let [r, g, b, _] = rgba(cx, "emit")?;
            material.emit = [r, g, b];
        }
        if self.shading == Shading::Pbr {
            material.metallic = cx.float("metallic")? as f32;
            material.roughness = cx.float("roughness")? as f32;
            let [r, g, b, _] = rgba(cx, "emit")?;
            material.emit = [r, g, b];
        }
        material.texture = cx.input_as::<Tex>(0).cloned();
        let material = Arc::new(material);
        // An occluder's picture would be empty: a dark matte sphere, a hole, stands in for it.
        let shown = if self.shading == Shading::Occluder {
            Arc::new(Material {
                shading: Shading::Phong,
                color: [0.06, 0.06, 0.06, 1.0],
                specular: [0.0; 3],
                ..Material::constant()
            })
        } else {
            material.clone()
        };
        let mut geo = Geo::single(self.sphere(gpu)?);
        for item in &mut geo.items {
            item.material = Some(shown.clone());
        }
        let mut extra = Vec::new();
        if self.shading == Shading::Pbr {
            match self.studio(gpu) {
                Ok(maps) => {
                    let mut light = Light::ambient([0.35; 3]);
                    light.kind = LightKind::Environment;
                    light.environment = Some(Environment { maps, turn: 0.0 });
                    extra.push(light);
                }
                Err(e) => log::warn!("material preview: {e}"),
            }
        }
        let kind = crate::geo::Preview::Material;
        let preview = match crate::geo::preview_lit(gpu, &mut self.renderer, &geo, kind, &extra) {
            Ok(t) => Some(t),
            Err(e) => {
                log::warn!("material preview: {e}");
                None
            }
        };
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Mat, Mat { material, preview })]))
    }
}
