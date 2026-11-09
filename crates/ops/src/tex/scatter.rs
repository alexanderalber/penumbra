//! `tex.scatter`: points from a texture added into a picture. Every texel of the `positions`
//! input is a point; it adds 1 (or its texel of `values`, or `value`) into the pixel it falls
//! on. The drawing is in [`penumbra_gpu::scatter`]: exact sums in 32-bit floats, no camera, no
//! material, no geometry.

use penumbra_core::{Error, ParamSpec, Value};
use penumbra_gpu::{
    FLOAT_FORMAT, FORMAT, Gpu, Scatter, ScatterDead, ScatterEdge, ScatterSettings, ScatterUnits, Tex, TexRing,
};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, Density, FnNodeType, NodeDef, NodeDesc, NodeTypes,
    Operator,
};

use super::{gpu, side};
use crate::on_gpu;

pub const SCATTER: &str = "tex.scatter";

fn desc() -> NodeDesc {
    NodeDesc::new(
        SCATTER,
        "Points from a texture added into a picture: each texel of `positions` adds 1 into the pixel at its x and y. For particles that leave a trail, and for counting where things are",
    )
    .param(super::width_param())
    .param(super::height_param())
    .param(super::keep_size_default(true))
    .param(
        ParamSpec::choice("units", &["pixels", "share"])
            .doc("What red (x) and green (y) of `positions` count in")
            .choice_doc("pixels", "pixels from the top left corner, y down: a point at 10.7, 3.2 adds into pixel 10, 3")
            .choice_doc("share", "shares of the picture, 0 to 1 across and 0 to 1 down"),
    )
    .param(
        ParamSpec::choice("edge", &["drop", "wrap", "clamp"])
            .doc("What happens to a point beyond the picture's edge")
            .choice_doc("drop", "it adds nothing")
            .choice_doc("wrap", "it comes back in from the opposite side, as on a torus")
            .choice_doc("clamp", "it adds into the nearest pixel of the edge"),
    )
    .param(
        ParamSpec::choice("dead", &["none", "blue", "alpha"])
            .doc("Which channel of `positions` marks a dead point: a point with that channel below 0 adds nothing")
            .choice_doc("none", "only points whose x or y is not a finite number add nothing")
            .choice_doc("blue", "a point with blue below 0 adds nothing")
            .choice_doc("alpha", "a point with alpha below 0 adds nothing"),
    )
    .param(
        ParamSpec::vec4("value", [1.0, 1.0, 1.0, 1.0])
            .doc("What each point adds to r, g, b and a while `values` is not connected"),
    )
    .param(
        ParamSpec::int("splat", 1)
            .clamp(1.0, f64::from(penumbra_gpu::scatter::MAX_SPLAT))
            .more()
            .doc("Each point adds into a square of this many pixels a side around its pixel. Costs the square of it"),
    )
    .param(
        ParamSpec::choice("format", &["32f", "16f"])
            .more()
            .doc("Number format of the picture")
            .choice_doc("32f", "32-bit floats: sums are exact up to 16 777 216 per pixel")
            .choice_doc("16f", "half floats: counts are exact up to 2048 per pixel; half the memory"),
    )
    .input("positions", DataKind::Tex)
    .input("values", DataKind::Tex)
    .output("out", DataKind::Tex)
    .describe_input(
        "positions",
        "One point per texel: red is x, green is y. Make it with a tex.shader with `format` 32f, so positions keep fractions of a pixel",
    )
    .describe_input(
        "values",
        "What each point adds, from the texel at the same place as its position: all four channels. Optional",
    )
    .describe_output("out", "The sum of all points per pixel. 0 where no point landed")
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(
        FnNodeType::new(desc(), || Box::new(ScatterOp::default()) as Box<dyn Operator>)
            .warm_up(on_gpu(warm_up)),
    )
}

/// Compiles the pipelines ahead of the first node.
fn warm_up(gpu: &Gpu) -> Result<(), Error> {
    Scatter::new().warm_up(gpu)
}

#[derive(Default)]
struct ScatterOp {
    out: TexRing,
    drawer: Scatter,
}

fn settings(cx: &CookContext<'_>) -> Result<ScatterSettings, CookError> {
    let units = match cx.text("units")? {
        "share" => ScatterUnits::Share,
        _ => ScatterUnits::Pixels,
    };
    let edge = match cx.text("edge")? {
        "wrap" => ScatterEdge::Wrap,
        "clamp" => ScatterEdge::Clamp,
        _ => ScatterEdge::Drop,
    };
    let dead = match cx.text("dead")? {
        "blue" => ScatterDead::Blue,
        "alpha" => ScatterDead::Alpha,
        _ => ScatterDead::None,
    };
    let splat = u32::try_from(cx.int("splat")?).unwrap_or(1);
    let value = match cx.param("value") {
        Some(Value::Vec4(v)) => v.map(|v| v as f32),
        _ => [1.0; 4],
    };
    Ok(ScatterSettings { units, edge, dead, splat, value })
}

/// Whether the picture is to be 32-bit floats: `format` asks for them and the GPU can add into
/// them. Otherwise half floats, with a warning when 32f was asked for.
fn wants_float(cx: &CookContext<'_>, gpu: &Gpu) -> Result<bool, CookError> {
    if cx.text("format")? != "32f" {
        return Ok(false);
    }
    if gpu.float_blending() {
        return Ok(true);
    }
    cx.warn("format 32f: this GPU cannot add into 32-bit floats, so the picture is 16f and counts stay exact up to 2048 per pixel");
    Ok(false)
}

impl Operator for ScatterOp {
    fn gpu_bytes(&self) -> u64 {
        self.out.bytes()
    }

    fn trim(&mut self) {
        self.out.trim();
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let wired = |i: usize| cx.def().inputs.get(i).copied().flatten().is_some();
        let positions = cx.input_as::<Tex>(0);
        let values = cx.input_as::<Tex>(1);
        // A wired input without a picture yet: wait for it rather than show an empty frame.
        if (wired(0) && positions.is_none()) || (wired(1) && values.is_none()) {
            return Err(CookError::Pending);
        }
        if let (Some(p), Some(v)) = (positions, values)
            && p.size() != v.size()
        {
            let ((pw, ph), (vw, vh)) = (p.size(), v.size());
            cx.warn(format!(
                "`values` is {vw}x{vh} and `positions` {pw}x{ph}: points beyond `values` take its last row or column"
            ));
        }
        let settings = settings(cx)?;
        let format = if wants_float(cx, gpu)? { FLOAT_FORMAT } else { FORMAT };
        let (w, h) = (side(cx, "width")?, side(cx, "height")?);
        let target = self.out.next_in(gpu, w, h, format, SCATTER)?;
        self.drawer.render(gpu, &target, positions, values, &settings)?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, target)]))
    }

    fn density(&self, _: &NodeDef, _: &NodeDesc, _: usize) -> Density {
        Density::Unrelated
    }

    fn sets_size(&self, _: &NodeDef, _: &NodeDesc) -> bool {
        true
    }
}
