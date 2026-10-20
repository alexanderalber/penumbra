//! `tex.shapes`: the shape layer. Tagged rectangles, ellipses and polygons in surface coordinates,
//! stored as text in `shapes` (see [`crate::shapes`]) and drawn in the viewer, become a mask and a
//! signed distance field of the tags `tag` names, outlines of all of them in their tag colours (to
//! see them on the wall while placing them), a table of every shape, and per pixel which of those
//! shapes it belongs to (what `tex.shape_content` animates).
//!
//! With its `table` input connected, the shapes come from that table instead (any table with
//! the columns the `table` output has), so a second node can mask other tags of the same shapes.
//!
//! `shapes.trace` (the trace row in its params) writes the outlines of a mask or label texture
//! into `shapes` as `poly` lines ([`crate::trace`]), to be edited by hand like drawn ones.

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
        ParamSpec::int("width", 1920).clamp(1.0, MAX_SIDE).curve(Curve::Log).unit(Unit::Pixels).doc(
            "Width of the surface in pixels (the mappers' input); shape coordinates are in these pixels",
        ),
    )
    .param(
        ParamSpec::int("height", 1080)
            .clamp(1.0, MAX_SIDE)
            .curve(Curve::Log)
            .unit(Unit::Pixels)
            .doc("Height of the surface in pixels; shape coordinates are in these pixels"),
    )
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
            .more()
            .doc("Width of the lines in the outline output, in surface pixels"),
    )
    .input("table", DataKind::Table)
    .output("mask", DataKind::Tex)
    .output("sdf", DataKind::Tex)
    .output("outline", DataKind::Tex)
    .output("table", DataKind::Table)
    .output("id", DataKind::Tex)
    .describe_output("mask", "White inside the shapes of the tags in `tag`")
    .describe_output("sdf", "Signed distance to the edge of those shapes")
    .describe_output("outline", "Outlines of all shapes in their tag colours, to see them while placing them")
    .describe_output("table", "Every shape as a row; another tex.shapes can read it at its `table` input")
    .describe_output(
        "id",
        "Data, not a picture: per pixel the shape of the tags in `tag` it is inside (else the nearest): \
         red its id (the table's `id`), green a random value 0..1 per shape, blue where along its \
         outline (0..1, clockwise), alpha the signed distance to its edge in pixels; for tex.shape_content",
    )
    .describe_input("table", "Shapes from a table (as the `table` output has them) instead of the node's own")
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
    rings: [TexRing; 4],
    drawer: Option<Drawer>,
}

/// A shape as drawn: its outline in target pixels, its colour and its id.
type Drawn = (Vec<[f32; 2]>, [f32; 4], u32);

fn as_drawn(polys: &[Drawn]) -> Vec<ShapePoly<'_>> {
    polys.iter().map(|(points, color, id)| ShapePoly { points, color: *color, id: *id }).collect()
}

/// An outline in target pixels, clockwise on screen (y down), so that runs along outlines
/// (`tex.shape_content`'s chase) go the same way round every shape.
fn outline(instance: &Instance, size: (u32, u32)) -> Vec<[f32; 2]> {
    let (w, h) = (f64::from(size.0), f64::from(size.1));
    let mut points: Vec<[f32; 2]> =
        instance.geom.outline().into_iter().map(|p| [(p[0] * w) as f32, (p[1] * h) as f32]).collect();
    // Twice the signed area; positive is clockwise with y down.
    let area: f64 = points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .map(|(a, b)| f64::from(a[0]) * f64::from(b[1]) - f64::from(b[0]) * f64::from(a[1]))
        .sum();
    if area < 0.0 {
        points.reverse();
    }
    points
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
        // Ids are row numbers of the table: positions among all instances.
        let all_polys: Vec<Drawn> = instances
            .iter()
            .zip(0u32..)
            .map(|(i, id)| {
                let [r, g, b] = shapes::tag_color(&tags, &i.tag);
                (outline(i, size), [r, g, b, 1.0], id)
            })
            .collect();
        let chosen_polys: Vec<Drawn> = all_polys
            .iter()
            .zip(&instances)
            .filter(|(_, i)| shapes::matches(&filter, &i.tag))
            .map(|(p, _)| p.clone())
            .collect();
        let width = cx.float("outline")? as f32;
        let drawer = self.drawer.get_or_insert_with(Drawer::new);
        let modes = [
            (ShapeMode::Mask, as_drawn(&chosen_polys)),
            // Distances in surface widths: one pixel is 1 / width.
            (ShapeMode::Sdf { unit: 1.0 / size.0 as f32 }, as_drawn(&chosen_polys)),
            (ShapeMode::Outline { width }, as_drawn(&all_polys)),
            (ShapeMode::Id, as_drawn(&chosen_polys)),
        ];
        let mut outputs = Vec::with_capacity(5);
        for (ring, (mode, polys)) in self.rings.iter_mut().zip(modes) {
            let target: Tex = ring.next(gpu, size.0, size.1, SHAPES)?;
            drawer.render(gpu, &target, &polys, mode)?;
            outputs.push(Data::new(DataKind::Tex, target));
        }
        // The table goes between the outline and the ids (outputs keep their numbers).
        outputs.insert(3, Data::new(DataKind::Table, shapes::table(&instances)?));
        Ok(Cooked::Outputs(outputs))
    }
}
