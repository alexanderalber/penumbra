//! `tex.shapes`: the shape layer. Tagged rectangles, ellipses and polygons in surface coordinates,
//! stored as text in `shapes` (see [`crate::shapes`]) and drawn in the viewer, become a mask and a
//! signed distance field of the tags `tag` names, outlines of all of them in their tag colours (to
//! see them on the wall while placing them), and a table of every shape.
//!
//! With its `table` input connected, the shapes come from that table instead (any table with
//! the columns the `table` output has), so a second node can mask other tags of the same shapes.

use penumbra_core::{Curve, Error, ParamSpec, Unit};
use penumbra_gpu::{ShapeMode, ShapePoly, Shapes as Drawer, Tex, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDef, NodeDesc, NodeTypes, Operator,
};

use super::{MAX_SIDE, gpu, side};
use crate::shapes::{self, Instance};

pub const SHAPES: &str = "tex.shapes";

fn desc() -> NodeDesc {
    NodeDesc::new(
        SHAPES,
        "Tagged shapes on the surface: a mask and a distance field per tag, outlines, a table",
    )
    .param(
        ParamSpec::int("width", 1920)
            .clamp(1.0, MAX_SIDE)
            .curve(Curve::Log)
            .unit(Unit::Pixels)
            .doc("The surface's resolution (the mappers' input)"),
    )
    .param(ParamSpec::int("height", 1080).clamp(1.0, MAX_SIDE).curve(Curve::Log).unit(Unit::Pixels))
    .param(super::keep_size())
    .param(ParamSpec::text("shapes", "").multiline().doc(
        "One per line: `tag rect x,y w,h`, `tag ellipse x,y w,h` or `tag poly x,y x,y ...`, \
             optionally `repeat CxR dx,dy`; surface coordinates (draw them in the viewer)",
    ))
    .param(
        ParamSpec::text("tag", "")
            .doc("Tags in the mask and the distance field, separated by spaces; empty: all"),
    )
    .param(
        ParamSpec::float("outline", 2.0)
            .clamp(0.0, 64.0)
            .range(0.5, 8.0)
            .unit(Unit::Pixels)
            .rarely_used()
            .doc("Width of the outlines"),
    )
    .input("table", DataKind::Table)
    .output("mask", DataKind::Tex)
    .output("sdf", DataKind::Tex)
    .output("outline", DataKind::Tex)
    .output("table", DataKind::Table)
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(desc(), || Box::new(ShapesOp::default()) as Box<dyn Operator>))
}

/// Compiles the pipeline ahead of the first node.
pub fn warm_up(gpu: &penumbra_gpu::Gpu) -> Result<(), Error> {
    Drawer::new().warm_up(gpu)
}

/// The shapes a node draws: from its `table` input when connected, else its `shapes` text.
pub fn source(cx: &CookContext<'_>) -> Result<Vec<shapes::Shape>, CookError> {
    if cx.def().inputs.first().copied().flatten().is_some() {
        let table = cx.input_as::<penumbra_core::Table>(0).ok_or(CookError::Pending)?;
        if !cx.text("shapes")?.trim().is_empty() {
            cx.warn("`shapes` is ignored while the table input is connected");
        }
        return shapes::from_table(table).map_err(CookError::Failed);
    }
    shapes::parse(cx.text("shapes")?).map_err(CookError::Failed)
}

#[derive(Default)]
struct ShapesOp {
    rings: [TexRing; 3],
    drawer: Option<Drawer>,
}

fn as_drawn(polys: &[(Vec<[f32; 2]>, [f32; 4])]) -> Vec<ShapePoly<'_>> {
    polys.iter().map(|(points, color)| ShapePoly { points, color: *color }).collect()
}

/// Outlines in target pixels.
fn outlines(instances: &[&Instance], size: (u32, u32)) -> Vec<Vec<[f32; 2]>> {
    let (w, h) = (f64::from(size.0), f64::from(size.1));
    instances
        .iter()
        .map(|i| i.geom.outline().into_iter().map(|p| [(p[0] * w) as f32, (p[1] * h) as f32]).collect())
        .collect()
}

impl Operator for ShapesOp {
    fn sets_size(&self, _def: &NodeDef, _desc: &NodeDesc) -> bool {
        true
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let size = (side(cx, "width")?, side(cx, "height")?);
        let all = source(cx)?;
        let tags = shapes::tags(&all);
        let instances = shapes::instances(&all);
        let filter = shapes::filter(cx.text("tag")?);
        for f in &filter {
            if !tags.contains(f) {
                cx.warn(format!("no shape has the tag `{f}`"));
            }
        }
        let every: Vec<&Instance> = instances.iter().collect();
        let chosen: Vec<&Instance> = instances.iter().filter(|i| shapes::matches(&filter, &i.tag)).collect();
        let (lines_all, lines_chosen) = (outlines(&every, size), outlines(&chosen, size));
        let polys = |lines: &[Vec<[f32; 2]>], of: &[&Instance]| -> Vec<(Vec<[f32; 2]>, [f32; 4])> {
            lines
                .iter()
                .zip(of)
                .map(|(l, i)| {
                    let [r, g, b] = shapes::tag_color(&tags, &i.tag);
                    (l.clone(), [r, g, b, 1.0])
                })
                .collect()
        };
        let chosen_polys = polys(&lines_chosen, &chosen);
        let all_polys = polys(&lines_all, &every);
        let width = cx.float("outline")? as f32;
        let drawer = self.drawer.get_or_insert_with(Drawer::new);
        let modes = [
            (ShapeMode::Mask, as_drawn(&chosen_polys)),
            // Distances in surface widths: one pixel is 1 / width.
            (ShapeMode::Sdf { unit: 1.0 / size.0 as f32 }, as_drawn(&chosen_polys)),
            (ShapeMode::Outline { width }, as_drawn(&all_polys)),
        ];
        let mut outputs = Vec::with_capacity(4);
        for (ring, (mode, polys)) in self.rings.iter_mut().zip(modes) {
            let target: Tex = ring.next(gpu, size.0, size.1, SHAPES)?;
            drawer.render(gpu, &target, &polys, mode)?;
            outputs.push(Data::new(DataKind::Tex, target));
        }
        outputs.push(Data::new(DataKind::Table, shapes::table(&instances)?));
        Ok(Cooked::Outputs(outputs))
    }
}
