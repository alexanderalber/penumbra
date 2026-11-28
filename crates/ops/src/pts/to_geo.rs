//! `geo.from_pts`: points as geometry that `tex.render` draws. Each point becomes a sprite (a
//! small disc or square that faces the camera) or a copy of a model, all in one instanced draw;
//! or the points' lines ([`Strips`]) become ribbons that face the camera, one instance per
//! segment. A sprite is a square of two triangles; a disc and a soft edge are cut out of it by
//! the renderer's fragment stage ([`Instances::round`], [`Instances::softness`]), so a million
//! discs cost what a million squares cost. With `blend` add the copies add their colour to what
//! lies behind them ([`Instances::add`]). The attributes the drawing reads (`position`, `scale` or `width`, `color`, `normal`)
//! are copied on the GPU into pictures of 32-bit floats, one texel per point, for the renderer's
//! instances. The pictures are as large as the points' capacity, so they keep their size while
//! the count moves. Where the count is on the GPU, the renderer reads it there and draws exactly
//! the points that are alive.
//!
//! The box around the points (for framing and shadow maps) is exact where the points carry
//! bounds. Points a program moved have none: then a reduction on the GPU finds it, read back a
//! frame or two late and grown by a tenth ([`LateBounds`]). Nothing is culled by it.

use std::sync::Arc;

use penumbra_core::{Curve, Error, ParamSpec};
use penumbra_gpu::geo::{IDENTITY, Instances, Item, Lines, Mat4, Model};
use penumbra_gpu::points::reach::{LateBounds, LateMost};
use penumbra_gpu::points::{Count, LINE, Strips};
use penumbra_gpu::{BufferRing, Chain, Geo, Gpu, Points, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};
use penumbra_mesh::{Builder, Limits, Mesh, Vertex, prim};

use super::program::wake;
use crate::gpu;

pub const KIND: &str = "geo.from_pts";

/// Choices of `mode`, `sprite` and `orient`.
pub const MODES: [&str; 3] = ["sprites", "copies", "lines"];
pub const SPRITES: [&str; 2] = ["disc", "square"];
pub const ORIENTS: [&str; 2] = ["none", "normal"];
pub const BLENDS: [&str; 2] = ["over", "add"];

/// Corners around the disc a file gets for a round sprite (`out.mesh`): the renderer cuts its
/// discs from a square instead.
const DISC_SEGMENTS: u32 = 16;

/// Half the side of the box that holds every point where the points do not say and no box has
/// been read back from the GPU yet: framing and shadow maps use it.
const UNKNOWN_EXTENT: f32 = 10.0;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            KIND,
            "Draws points with tex.render: a small disc or square facing the camera at every point, a copy of a model at every point, or ribbons along the points' lines, all on the GPU",
        )
        .param(
            ParamSpec::choice("mode", &MODES)
                .doc("What stands at each point")
                .choice_doc("sprites", "a flat disc or square that always faces the camera")
                .choice_doc("copies", "a copy of the geometry on the `model` input")
                .choice_doc("lines", "a ribbon facing the camera from each point to the next along the points' lines (a trail, a generated line or circle). Points without lines make one line in their order"),
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
            ParamSpec::float("softness", 0.0)
                .clamp(0.0, 1.0)
                .range(0.0, 1.0)
                .doc("sprites: how far in from the outline each sprite fades out. 0 is a hard edge. 1 fades all the way from the centre: a soft glowing dot, as a point sprite with a falloff texture in TouchDesigner. With `blend` over a soft edge is see-through: the sprites are drawn two or three times. With `blend` add they are drawn once")
                .applies_when("mode", &["sprites"]),
        )
        .param(
            ParamSpec::choice("blend", &BLENDS)
                .doc("How the copies meet what lies behind them")
                .choice_doc("over", "they cover it, as any surface: opaque where the colour is, see-through where its alpha or a soft edge says")
                .choice_doc("add", "their colour is added to it, so overlapping sprites or lines grow brighter: glow, sparks, light trails. Drawn after everything else, hidden by what stands in front, casting no shadow. They cover nothing: the render stays see-through there, and tex.composite over a picture adds their light to it"),
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
            ParamSpec::float("width", 0.01)
                .clamp(0.0, 1e4)
                .range(0.0005, 0.5)
                .curve(Curve::Log)
                .doc("lines: the width of the ribbons in metres, times the points' `width` (LineWidth in TouchDesigner), blended between the two ends of each segment")
                .applies_when("mode", &["lines"]),
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
        .describe_input("in", "The points. `position` places each sprite, copy or line point, `scale` sizes a sprite or copy, `width` a line, `color` multiplies the colour. Without `scale` or `width` every point counts as 1, without `color` as white")
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
    /// Pictures of `position`, `scale`, `color` and `normal` (for lines of any length the place
    /// along each line instead).
    textures: [TexRing; 4],
    scratch: BufferRing,
    /// The square every sprite is drawn from, disc or not.
    sprite: Option<Arc<Model>>,
    /// The quad each segment of a line is drawn from.
    segment: Option<Arc<Model>>,
    baked: Vec<Baked>,
    /// The box of points without exact bounds, read back from the GPU.
    late: LateBounds,
    /// The largest `scale` or `width`, read back from the GPU.
    most: LateMost,
}

impl FromPts {
    fn sprite(&mut self, gpu: &Gpu) -> Result<Arc<Model>, Error> {
        if let Some(m) = &self.sprite {
            return Ok(m.clone());
        }
        let (model, _) = Model::upload(gpu, prim::plane([1.0, 1.0])?, "sprite")?;
        let model = Arc::new(model);
        self.sprite = Some(model.clone());
        Ok(model)
    }

    fn segment(&mut self, gpu: &Gpu) -> Result<Arc<Model>, Error> {
        if let Some(m) = &self.segment {
            return Ok(m.clone());
        }
        let (model, _) = Model::upload(gpu, segment()?, "line segment")?;
        let model = Arc::new(model);
        self.segment = Some(model.clone());
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

/// A disc of diameter 1 upright in the xz plane, facing -y, the texture over its square: what a
/// round sprite is in a file.
pub(crate) fn disc() -> Result<Mesh, Error> {
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

/// The quad a segment of a line is drawn from: x 0 at the segment's start and 1 at its end, z
/// -0.5 and 0.5 its two sides; the renderer puts the corners where the line runs.
fn segment() -> Result<Mesh, Error> {
    let limits = Limits::default();
    let mut b = Builder::new(&limits);
    let part = b.part("line")?;
    let normal = [0.0, -1.0, 0.0];
    let mut corners = Vec::with_capacity(4);
    for (x, z) in [(0.0, -0.5), (1.0, -0.5), (1.0, 0.5), (0.0, 0.5)] {
        corners.push(b.vertex(Vertex { position: [x, 0.0, z], normal, uv: [x, z + 0.5] })?);
    }
    let [c0, c1, c2, c3] = corners[..] else { return Err(Error::new("a segment has four corners")) };
    // Counter-clockwise seen from the viewer, where the renderer turns the ribbon.
    b.triangle([c3, c2, c1], part, 0)?;
    b.triangle([c3, c1, c0], part, 0)?;
    Ok(b.finish())
}

/// The lines the renderer draws through `points`: one set for each way their strips run. With
/// no strips, one line through every point.
pub fn lines_of(points: &Points) -> Vec<Lines> {
    let count = points.count();
    let (limit, live) = match &count {
        Count::Known(n) => (*n, false),
        _ => (u32::MAX, true),
    };
    let line = |len: u32, stride: u32, closed: bool| Lines {
        len,
        stride,
        closed,
        pairs: false,
        runs: false,
        limit,
        live,
    };
    let pairs = Lines { pairs: true, ..line(2, 1, false) };
    match points.strips() {
        Strips::Pairs => vec![pairs],
        // Lines of any length: each point says where it lies on its own ([`LINE`]).
        Strips::Runs { closed } if points.attr(LINE).is_some() => {
            vec![Lines { runs: true, ..line(0, 1, closed) }]
        }
        // Lines of two points with a count the CPU knows: a copy per line, none spent on a first
        // point.
        Strips::Regular { len: 2, closed: false } if !live => vec![pairs],
        Strips::Regular { len, closed } => vec![line(len, 1, closed)],
        Strips::Grid { cols, rows, columns } => {
            let cols = cols.max(1);
            let mut out = Vec::new();
            if rows {
                out.push(line(cols, 1, false));
            }
            if columns {
                out.push(line(count.at_most().div_ceil(cols), cols, false));
            }
            out
        }
        // One line through all: as long as the count where it is known, else until the first
        // point past the count on the GPU.
        _ => vec![line(if live { 0 } else { limit }, 1, false)],
    }
}

impl Operator for FromPts {
    fn gpu_bytes(&self) -> u64 {
        let textures: u64 = self.textures.iter().map(TexRing::bytes).sum();
        let sprite =
            self.sprite.as_ref().map_or(0, |m| m.bytes()) + self.segment.as_ref().map_or(0, |m| m.bytes());
        let baked: u64 = self.baked.iter().map(|b| b.made.bytes()).sum();
        textures + self.scratch.bytes() + sprite + baked + self.late.bytes() + self.most.bytes()
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
        let mode = cx.text("mode")?;
        let (copies, lines) = (mode == "copies", mode == "lines");
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
        // The pictures, the reductions and their readbacks all go into one command encoder: one
        // each would cost the CPU more than their GPU work.
        let mut chain = Chain::new(KIND);
        let [position, scale, color, normal] = &mut self.textures;
        let Some(position) =
            points.attr_texture_in(&mut chain, gpu, "position", position, &mut self.scratch)?
        else {
            cx.warn("the points have no `position`: nothing is drawn");
            return empty();
        };
        // A line's width comes from `width`, a sprite's or a copy's size from `scale`.
        let sized_by = if lines { "width" } else { "scale" };
        let scale = points.attr_texture_in(&mut chain, gpu, sized_by, scale, &mut self.scratch)?;
        let color = points.attr_texture_in(&mut chain, gpu, "color", color, &mut self.scratch)?;
        // Lines of any length take the place along each line where copies take their aim.
        let runs = lines && matches!(points.strips(), Strips::Runs { .. });
        let aim = if copies && cx.text("orient")? == "normal" {
            points.attr_texture_in(&mut chain, gpu, "normal", normal, &mut self.scratch)?
        } else if runs {
            points.attr_texture_in(&mut chain, gpu, LINE, normal, &mut self.scratch)?
        } else {
            None
        };
        // Exact bounds where the points know them; else the box read back from the GPU, late.
        let late = match points.bounds() {
            Some(_) => None,
            None => self.late.update_in(&mut chain, gpu, points, Some(wake(cx)))?,
        };
        // The widest line or largest sprite, so the box holds them (framing, shadow maps). Until
        // it arrives, 1: the size param alone.
        let scale_most = match &scale {
            Some(_) => self.most.update_in(&mut chain, gpu, points, sized_by, Some(wake(cx)))?,
            None => None,
        };
        chain.run(gpu)?;
        let extent = points
            .bounds()
            .or(late)
            .map_or(UNKNOWN_EXTENT, |b| b.min.iter().chain(&b.max).map(|v| v.abs()).fold(0.0, f32::max));
        let size = match mode {
            "copies" => cx.float("scale")?,
            "lines" => cx.float("width")?,
            _ => cx.float("size")?,
        } as f32;
        let instances = Instances {
            count: Some(count),
            // A count on the GPU: the renderer draws that many, none of the dead points.
            live: points.count().header().cloned(),
            size,
            face_camera: !copies && !lines,
            aim,
            scale,
            scale_most: scale_most.unwrap_or(1.0),
            color,
            extent,
            late: late.is_some(),
            guess: points.bounds().is_none() && late.is_none(),
            round: !copies && !lines && cx.text("sprite")? == "disc",
            softness: if copies || lines { 0.0 } else { cx.float("softness")? as f32 },
            add: cx.text("blend")? == "add",
            ..Instances::of(position)
        };
        if lines {
            self.baked.clear();
            let ways = lines_of(points);
            if ways.iter().all(|l| l.len == 1) && count > 0 {
                cx.warn("every line has one point: nothing is drawn");
            }
            let model = self.segment(gpu)?;
            let items = ways
                .into_iter()
                .map(|l| {
                    // Every two points a line: a copy per line.
                    let copies = if l.pairs { count / 2 } else { count };
                    Item {
                        model: model.clone(),
                        world: IDENTITY,
                        part_offset: 0,
                        material: None,
                        instances: Some(Arc::new(Instances {
                            lines: Some(l),
                            count: Some(copies),
                            ..instances.clone()
                        })),
                    }
                })
                .collect();
            return Ok(Cooked::Outputs(vec![Data::new(DataKind::Geo, Geo { items, preview: None })]));
        }
        let instances = Arc::new(instances);
        let items = match model {
            None => {
                // No copies: the models baked for them are not needed any more.
                self.baked.clear();
                let model = self.sprite(gpu)?;
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
