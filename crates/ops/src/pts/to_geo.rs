//! `geo.from_pts`: points as geometry that `tex.render` draws. Each point becomes a sprite (a
//! small disc or square that faces the camera) or a copy of a model, all in one instanced draw.
//! The attributes the drawing reads (`position`, `scale`, `color`, `normal`) are copied on the
//! GPU into pictures of 32-bit floats, one texel per point, for the renderer's instances. The
//! pictures are as large as the points' capacity, so they keep their size while the count
//! moves. Where the count is on the GPU, the renderer reads it there and draws exactly the
//! points that are alive.

use std::sync::Arc;

use penumbra_core::{Curve, Error, ParamSpec};
use penumbra_gpu::geo::{IDENTITY, Instances, Item, Mat4, Model};
use penumbra_gpu::{BufferRing, Geo, Gpu, Points, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};
use penumbra_mesh::{Builder, Limits, Mesh, Vertex, prim};

use crate::gpu;

pub const KIND: &str = "geo.from_pts";

/// Choices of `mode`, `sprite` and `orient`.
pub const MODES: [&str; 2] = ["sprites", "copies"];
pub const SPRITES: [&str; 2] = ["disc", "square"];
pub const ORIENTS: [&str; 2] = ["none", "normal"];

/// Corners around a disc sprite.
const DISC_SEGMENTS: u32 = 16;

/// Half the side of the box that holds every point where the points do not say: framing and
/// shadow maps use it.
const UNKNOWN_EXTENT: f32 = 10.0;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            KIND,
            "Draws points with tex.render: a small disc or square facing the camera at every point, or a copy of a model at every point, all on the GPU",
        )
        .param(
            ParamSpec::choice("mode", &MODES)
                .doc("What stands at each point")
                .choice_doc("sprites", "a flat disc or square that always faces the camera")
                .choice_doc("copies", "a copy of the geometry on the `model` input"),
        )
        .param(
            ParamSpec::float("size", 0.02)
                .clamp(0.0, 1e4)
                .range(0.001, 1.0)
                .curve(Curve::Log)
                .doc("sprites: the width of each sprite in metres, times the point's `scale`")
                .applies_when("mode", &["sprites"]),
        )
        .param(
            ParamSpec::choice("sprite", &SPRITES)
                .doc("sprites: the sprite's outline")
                .choice_doc("disc", "a round disc, as wide as `size`")
                .choice_doc("square", "a square of side `size`")
                .applies_when("mode", &["sprites"]),
        )
        .param(
            ParamSpec::float("scale", 1.0)
                .clamp(0.0, 1e6)
                .range(0.01, 10.0)
                .curve(Curve::Log)
                .doc("copies: multiplies the model's size at every point, times the point's `scale`")
                .applies_when("mode", &["copies"]),
        )
        .param(
            ParamSpec::choice("orient", &ORIENTS)
                .doc("copies: how each copy is turned")
                .choice_doc("none", "every copy keeps the model's own turn")
                .choice_doc("normal", "each copy turns its +y to the point's `normal`, keeping +z up as far as it can")
                .applies_when("mode", &["copies"]),
        )
        .input("in", DataKind::Pts)
        .input("model", DataKind::Geo)
        .describe_input("in", "The points. `position` places each sprite or copy, `scale` sizes it, `color` multiplies its colour. Without `scale` every point counts as 1, without `color` as white")
        .describe_input("model", "copies: the geometry put at every point, as it stands around its origin")
        .output("out", DataKind::Geo),
        || Box::new(FromPts::default()) as Box<dyn Operator>,
    ))
}

/// A model with its item's placement baked in, kept while its source is drawn.
struct Baked {
    source: Arc<Model>,
    world: Mat4,
    made: Arc<Model>,
}

#[derive(Default)]
struct FromPts {
    /// Pictures of `position`, `scale`, `color` and `normal`.
    textures: [TexRing; 4],
    scratch: BufferRing,
    /// The sprite's model and whether it is the square.
    sprite: Option<(bool, Arc<Model>)>,
    baked: Vec<Baked>,
}

impl FromPts {
    fn sprite(&mut self, gpu: &Gpu, square: bool) -> Result<Arc<Model>, Error> {
        if let Some((s, m)) = &self.sprite
            && *s == square
        {
            return Ok(m.clone());
        }
        let mesh = if square { prim::plane([1.0, 1.0])? } else { disc()? };
        let (model, _) = Model::upload(gpu, mesh, "sprite")?;
        let model = Arc::new(model);
        self.sprite = Some((square, model.clone()));
        Ok(model)
    }

    /// `item`'s model with its placement in it, so the points place the copies in the world
    /// and a turn or a scale on the model stays the model's own.
    fn baked(&self, gpu: &Gpu, item: &Item, kept: &mut Vec<Baked>) -> Result<Arc<Model>, Error> {
        if item.world == IDENTITY {
            return Ok(item.model.clone());
        }
        let found = self
            .baked
            .iter()
            .chain(kept.iter())
            .find(|b| Arc::ptr_eq(&b.source, &item.model) && b.world == item.world)
            .map(|b| b.made.clone());
        let made = match found {
            Some(m) => m,
            None => Arc::new(Model::upload(gpu, item.model.mesh.transformed(&item.world), "copied model")?.0),
        };
        if !kept.iter().any(|b| Arc::ptr_eq(&b.made, &made)) {
            kept.push(Baked { source: item.model.clone(), world: item.world, made: made.clone() });
        }
        Ok(made)
    }
}

/// A disc of diameter 1 upright in the xz plane, facing -y, the texture over its square.
fn disc() -> Result<Mesh, Error> {
    let limits = Limits::default();
    let mut b = Builder::new(&limits);
    let part = b.part("sprite")?;
    let normal = [0.0, -1.0, 0.0];
    let centre = b.vertex(Vertex { position: [0.0; 3], normal, uv: [0.5, 0.5] })?;
    let mut rim = Vec::with_capacity(DISC_SEGMENTS as usize);
    for k in 0..DISC_SEGMENTS {
        let a = std::f32::consts::TAU * k as f32 / DISC_SEGMENTS as f32;
        let (x, z) = (0.5 * a.cos(), 0.5 * a.sin());
        rim.push(b.vertex(Vertex { position: [x, 0.0, z], normal, uv: [0.5 + x, 0.5 - z] })?);
    }
    for (k, a) in rim.iter().enumerate() {
        let Some(next) = rim.get((k + 1) % rim.len()) else { continue };
        // Counter-clockwise seen from the front.
        b.triangle([centre, *a, *next], part, 0)?;
    }
    Ok(b.finish())
}

impl Operator for FromPts {
    fn gpu_bytes(&self) -> u64 {
        let textures: u64 = self.textures.iter().map(TexRing::bytes).sum();
        let sprite = self.sprite.as_ref().map_or(0, |(_, m)| m.bytes());
        let baked: u64 = self.baked.iter().map(|b| b.made.bytes()).sum();
        textures + self.scratch.bytes() + sprite + baked
    }

    fn trim(&mut self) {
        for t in &mut self.textures {
            t.trim();
        }
        self.scratch.trim();
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let wired = |k: usize| cx.def().inputs.get(k).is_some_and(Option::is_some);
        let Some(points) = cx.input_as::<Points>(0) else {
            return Err(if wired(0) {
                CookError::Pending
            } else {
                CookError::failed("connect points to `in`")
            });
        };
        let copies = cx.text("mode")? == "copies";
        let model = if copies {
            match cx.input_as::<Geo>(1) {
                Some(g) => Some(g),
                None if wired(1) => return Err(CookError::Pending),
                None => return Err(CookError::failed("copies: connect a model to `model`")),
            }
        } else {
            None
        };
        let count = points.count().at_most();
        let empty = || Ok(Cooked::Outputs(vec![Data::new(DataKind::Geo, Geo::default())]));
        if count == 0 {
            return empty();
        }
        let [position, scale, color, normal] = &mut self.textures;
        let Some(position) = points.attr_texture(gpu, "position", position, &mut self.scratch)? else {
            cx.warn("the points have no `position`: nothing is drawn");
            return empty();
        };
        let scale = points.attr_texture(gpu, "scale", scale, &mut self.scratch)?;
        let color = points.attr_texture(gpu, "color", color, &mut self.scratch)?;
        let aim = if copies && cx.text("orient")? == "normal" {
            points.attr_texture(gpu, "normal", normal, &mut self.scratch)?
        } else {
            None
        };
        let extent = points
            .bounds()
            .map_or(UNKNOWN_EXTENT, |b| b.min.iter().chain(&b.max).map(|v| v.abs()).fold(0.0, f32::max));
        let size = if copies { cx.float("scale")? } else { cx.float("size")? } as f32;
        let instances = Arc::new(Instances {
            count: Some(count),
            // A count on the GPU: the renderer draws that many, none of the dead points.
            live: points.count().header().cloned(),
            size,
            face_camera: !copies,
            aim,
            scale,
            color,
            extent,
            ..Instances::of(position)
        });
        let items = match model {
            None => {
                // No copies: the models baked for them are not needed any more.
                self.baked.clear();
                let model = self.sprite(gpu, cx.text("sprite")? == "square")?;
                vec![Item {
                    model,
                    world: IDENTITY,
                    part_offset: 0,
                    material: None,
                    instances: Some(instances),
                }]
            }
            Some(geo) => {
                if geo.items.iter().any(|i| i.instances.is_some()) {
                    cx.warn("the model is instanced already: its instances are replaced");
                }
                let mut kept = Vec::new();
                let mut items = Vec::with_capacity(geo.items.len());
                for item in &geo.items {
                    let model = self.baked(gpu, item, &mut kept)?;
                    items.push(Item {
                        model,
                        world: IDENTITY,
                        instances: Some(instances.clone()),
                        ..item.clone()
                    });
                }
                self.baked = kept;
                items
            }
        };
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Geo, Geo { items, preview: None })]))
    }
}
