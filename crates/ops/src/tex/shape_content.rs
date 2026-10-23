//! `tex.shape_content`: content per shape from `tex.shapes`' `id` output, one node with a `mode`
//! instead of a generator per effect. Every shape gets its own colour and phase from its id and
//! random value, so a dozen shapes differ without a node each.

use penumbra_core::{ParamSpec, Unit};
use penumbra_graph::{DataKind, NodeDesc};

use super::{Builtin, Size, same};

pub(super) fn builtin() -> Builtin {
    let mode = ParamSpec::choice("mode", &["pulse", "chase", "ripple", "fill"])
        .doc("What each shape shows")
        .choice_doc("pulse", "the whole shape fades up and down")
        .choice_doc("chase", "lit segments run clockwise along the outline")
        .choice_doc("ripple", "rings run inwards from the edge")
        .choice_doc("fill", "the shape in its colour, still");
    Builtin {
        desc: NodeDesc::new("tex.shape_content", "Animates the shapes of a tex.shapes one by one: pulses, light running along the outlines, ripples")
            .param(mode)
            .param(ParamSpec::color("color_a", [1.0, 0.45, 0.1, 1.0]).doc("Colour of the shapes; with variety, one end of their range"))
            .param(ParamSpec::color("color_b", [0.15, 0.35, 1.0, 1.0]).doc("The other end of the shapes' colour range"))
            .param(
                ParamSpec::float("variety", 1.0)
                    .range(0.0, 1.0)
                    .doc("How far each shape's colour lies from color_a towards color_b, at random: 0 all color_a, 1 anywhere between"),
            )
            .param(
                ParamSpec::float("speed", 0.5)
                    .range(-4.0, 4.0)
                    .unit(Unit::Hertz)
                    .doc("Cycles per second: pulses, segments passing a point, rings starting; negative runs backwards"),
            )
            .param(
                ParamSpec::float("stagger", 0.0)
                    .range(0.0, 1.0)
                    .doc("Phase added per shape id, in cycles: 0.1 makes shapes follow one another in table order"),
            )
            .param(
                ParamSpec::float("scatter", 1.0)
                    .range(0.0, 1.0)
                    .doc("Random phase per shape, in cycles: 0 all in step, 1 all out of step"),
            )
            .param(
                ParamSpec::float("depth", 1.0)
                    .range(0.0, 1.0)
                    .doc("Pulse: how far the shape fades; 0 not at all, 1 to nothing"),
            )
            .param(
                ParamSpec::float("segments", 2.0)
                    .clamp(0.0, 1000.0)
                    .range(1.0, 12.0)
                    .doc("Chase: lit segments per outline"),
            )
            .param(
                ParamSpec::float("line", 6.0)
                    .clamp(0.0, 1000.0)
                    .range(0.5, 32.0)
                    .unit(Unit::Pixels)
                    .doc("Chase: width of the line on the edge, in pixels of the id input"),
            )
            .param(
                ParamSpec::float("spacing", 24.0)
                    .clamp(0.01, 100000.0)
                    .range(2.0, 200.0)
                    .unit(Unit::Pixels)
                    .doc("Ripple: distance between rings, in pixels of the id input"),
            )
            .param(
                ParamSpec::float("duty", 0.3)
                    .range(0.0, 1.0)
                    .doc("Chase and ripple: lit part of each segment or ring"),
            )
            .param(
                ParamSpec::float("softness", 0.3)
                    .range(0.0, 1.0)
                    .more()
                    .doc("Chase and ripple: how soft the ends of segments and rings are; 0 hard"),
            )
            .input("id", DataKind::Tex)
            .output("out", DataKind::Tex)
            .describe_input("id", "tex.shapes' `id` output; the output has the same size"),
        source: include_str!("shape_content.wgsl"),
        size: Size::Input,
        animated: Some(|cx| cx.text("mode").is_ok_and(|m| m != "fill") && cx.float("speed").is_ok_and(|s| s != 0.0)),
        density: same,
        fold: false,
    }
}
