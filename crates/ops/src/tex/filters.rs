//! Single-pass `tex` filters after TouchDesigner's TOPs of the same names: flip, channel mix,
//! chroma key, cross, displace, edge, fit, lens distort, limit, lookup, matte, monochrome, remap,
//! resolution, slope, threshold, tile. Each is a WGSL file in this directory with its params
//! declared here; behaviour is ours where TD's differs.

use penumbra_core::{Curve, ParamSpec, Unit, Value};
use penumbra_graph::{CookContext, CookError, DataKind, Density, NodeDef, NodeDesc};

use super::{Builtin, EXTEND, MAX_SIDE, Size, Sizes, side};

/// Channels a filter can read one of.
const CHANNEL: [&str; 5] = ["luminance", "red", "green", "blue", "alpha"];

fn filter(kind: &str, summary: &str) -> NodeDesc {
    NodeDesc::new(kind, summary).input("in", DataKind::Tex).output("out", DataKind::Tex)
}

fn pass(desc: NodeDesc, source: &'static str) -> Builtin {
    Builtin { desc, source, size: Size::Input, animated: None, density: super::same, fold: false }
}

/// `width` x `height` of a node that sets its own frame, input or not.
fn own_size(cx: &CookContext<'_>, _input: (u32, u32)) -> Result<(u32, u32), CookError> {
    Ok((side(cx, "width")?, side(cx, "height")?))
}

fn size_params(desc: NodeDesc, more: bool) -> NodeDesc {
    let w = ParamSpec::int("width", 1920).clamp(1.0, MAX_SIDE).curve(Curve::Log).unit(Unit::Pixels);
    let h = ParamSpec::int("height", 1080).clamp(1.0, MAX_SIDE).curve(Curve::Log).unit(Unit::Pixels);
    let desc = if more { desc.param(w.more()).param(h.more()) } else { desc.param(w).param(h) };
    desc.param(super::keep_size())
}

/// A choice whose default is not its first entry (the shader numbers the entries in order).
fn choice_default(name: &str, choices: &[&str], default: &str) -> ParamSpec {
    let mut spec = ParamSpec::choice(name, choices);
    spec.default = Value::Text(default.to_string());
    spec
}

fn float_of(def: &NodeDef, desc: &NodeDesc, name: &str) -> Option<f64> {
    def.param(desc, name).and_then(|v| v.as_f64())
}

/// fit.wgsl differentiated: the input spans `shown` of the `out` frame, so it moves `out / shown`
/// of its width per output width.
fn fit_density(def: &NodeDef, desc: &NodeDesc, _: usize, sizes: Option<Sizes>) -> Density {
    let Some(Sizes { input, output }) = sizes else { return Density::SAME };
    let src = [f64::from(input.0).max(1.0), f64::from(input.1).max(1.0)];
    let ratio = [f64::from(output.0) / src[0], f64::from(output.1) / src[1]];
    let s = match def.param(desc, "fit").and_then(|v| v.as_str()) {
        Some("fill") => [ratio[0].max(ratio[1]); 2],
        Some("stretch") => ratio,
        Some("native") => [1.0; 2],
        _ => [ratio[0].min(ratio[1]); 2],
    };
    Density::Linear([[ratio[0] / s[0], 0.0], [0.0, ratio[1] / s[1]]])
}

pub(super) fn builtins() -> Vec<Builtin> {
    vec![
        Builtin {
            size: Size::Custom(|cx, (w, h)| Ok(if cx.bool("flop")? { (h, w) } else { (w, h) })),
            // Mirroring keeps the resolution; a flop swaps the axes.
            density: |def, desc, _, _| {
                if def.param(desc, "flop").and_then(|v| v.as_bool()) == Some(true) {
                    Density::Linear([[0.0, 1.0], [1.0, 0.0]])
                } else {
                    Density::SAME
                }
            },
            ..pass(
                filter("tex.flip", "Mirror left to right or top to bottom; flop swaps the axes")
                    .param(ParamSpec::bool("flip_x", false).doc("Left becomes right"))
                    .param(ParamSpec::bool("flip_y", false).doc("Top becomes bottom"))
                    .param(
                        ParamSpec::bool("flop", false)
                            .doc("Swap the axes (transpose); the output is as wide as the input is high"),
                    ),
                include_str!("flip.wgsl"),
            )
        },
        pass(
            filter("tex.channelmix", "Each output channel a weighted sum of the input's channels")
                .param(
                    ParamSpec::vec4("red", [1.0, 0.0, 0.0, 0.0])
                        .range(-2.0, 2.0)
                        .doc("Weights of r, g, b, a"),
                )
                .param(
                    ParamSpec::vec4("green", [0.0, 1.0, 0.0, 0.0])
                        .range(-2.0, 2.0)
                        .doc("Weights of r, g, b, a"),
                )
                .param(
                    ParamSpec::vec4("blue", [0.0, 0.0, 1.0, 0.0])
                        .range(-2.0, 2.0)
                        .doc("Weights of r, g, b, a"),
                )
                .param(
                    ParamSpec::vec4("alpha", [0.0, 0.0, 0.0, 1.0])
                        .range(-2.0, 2.0)
                        .doc("Weights of r, g, b, a"),
                )
                .param(
                    ParamSpec::vec4("bias", [0.0; 4])
                        .range(-1.0, 1.0)
                        .more()
                        .doc("Added to r, g, b, a afterwards"),
                ),
            include_str!("channel_mix.wgsl"),
        ),
        pass(
            filter("tex.chromakey", "Make pixels near a key colour transparent (green or blue screen)")
                .param(ParamSpec::color("key", [0.0, 1.0, 0.0, 1.0]).doc("The screen's colour"))
                .param(
                    ParamSpec::float("tolerance", 0.1)
                        .range(0.0, 0.5)
                        .doc("Chroma distance from the key that is fully transparent"),
                )
                .param(
                    ParamSpec::float("softness", 0.08)
                        .range(0.0, 0.5)
                        .doc("Width of the soft edge beyond it"),
                )
                .param(
                    ParamSpec::float("spill", 0.5)
                        .range(0.0, 1.0)
                        .doc("How much of the key's tint is removed from what stays"),
                )
                .param(ParamSpec::bool("invert", false).doc("Keep the key colour, drop the rest"))
                .param(
                    ParamSpec::choice("output", &["keyed", "matte"])
                        .doc("The keyed image, or its alpha as white on black"),
                ),
            include_str!("chroma_key.wgsl"),
        ),
        pass(
            NodeDesc::new("tex.cross", "Mix of two inputs; resolution of a")
                .param(ParamSpec::float("cross", 0.5).range(0.0, 1.0).doc("0 is all a, 1 all b"))
                .input("a", DataKind::Tex)
                .input("b", DataKind::Tex)
                .output("out", DataKind::Tex),
            include_str!("cross.wgsl"),
        ),
        pass(
            NodeDesc::new("tex.displace", "Push each pixel of the input by the values of a map")
                .param(
                    ParamSpec::choice("horizontal", &["red", "green", "blue", "alpha", "luminance", "none"])
                        .doc("Map channel that pushes left and right"),
                )
                .param(
                    choice_default(
                        "vertical",
                        &["red", "green", "blue", "alpha", "luminance", "none"],
                        "green",
                    )
                    .doc("Map channel that pushes up and down"),
                )
                .param(
                    ParamSpec::vec2("weight", [0.1, 0.1])
                        .range(-1.0, 1.0)
                        .doc("Push for a map value 1 above the midpoint, in image sizes"),
                )
                .param(ParamSpec::float("midpoint", 0.5).range(0.0, 1.0).doc("Map value that does not push"))
                .param(ParamSpec::choice("extend", &EXTEND).more().doc("Outside the input"))
                .input("in", DataKind::Tex)
                .input("map", DataKind::Tex)
                .output("out", DataKind::Tex),
            include_str!("displace.wgsl"),
        ),
        pass(
            filter("tex.edge", "Edges of the luminance (Sobel)")
                .param(ParamSpec::float("strength", 2.0).range(0.0, 10.0))
                .param(
                    ParamSpec::float("distance", 1.0)
                        .range(0.5, 8.0)
                        .unit(Unit::Pixels)
                        .doc("Distance of the neighbours compared"),
                )
                .param(ParamSpec::color("color", [1.0; 4]).doc("Colour of the edges"))
                .param(
                    ParamSpec::choice("mode", &["edges", "alpha", "over"])
                        .doc("On black, on transparent, or over the input"),
                ),
            include_str!("edge.wgsl"),
        ),
        Builtin {
            size: Size::Own,
            density: fit_density,
            fold: false,
            ..pass(
                size_params(
                    filter("tex.fit", "The input fitted into a frame of its own size, aspect kept or not"),
                    false,
                )
                .param(
                    ParamSpec::choice("fit", &["fit", "fill", "stretch", "native"]).doc(
                        "Whole and as large as fits, covering and cropped, stretched, or pixel for pixel",
                    ),
                )
                .param(
                    ParamSpec::vec2("justify", [0.5, 0.5])
                        .range(0.0, 1.0)
                        .doc("Where the image sits in the frame: 0 left or top, 1 right or bottom"),
                )
                .param(
                    ParamSpec::choice("extend", &["zero", "hold", "repeat", "mirror"])
                        .doc("Outside the image"),
                ),
                include_str!("fit.wgsl"),
            )
        },
        pass(
            filter("tex.lensdistort", "Radial lens distortion (barrel or pincushion) around a centre")
                .param(
                    ParamSpec::float("k1", 0.1)
                        .range(-1.0, 1.0)
                        .doc("Main term; positive pulls the edges in (barrel), negative pushes them out"),
                )
                .param(
                    ParamSpec::float("k2", 0.0).range(-1.0, 1.0).doc("Fourth-order term, for the far edges"),
                )
                .param(
                    ParamSpec::float("scale", 1.0).range(0.25, 4.0).curve(Curve::Log).doc("Zoom afterwards"),
                )
                .param(
                    ParamSpec::vec2("center", [0.5, 0.5])
                        .range(0.0, 1.0)
                        .more()
                        .doc("Optical centre; 0, 0 is the top left"),
                )
                .param(choice_default("extend", &EXTEND, "zero").doc("Outside the input")),
            include_str!("lens_distort.wgsl"),
        ),
        pass(
            filter("tex.limit", "Keep values within a range: clamp, wrap around or mirror")
                .param(ParamSpec::float("min", 0.0).range(-1.0, 2.0))
                .param(ParamSpec::float("max", 1.0).range(-1.0, 2.0))
                .param(
                    ParamSpec::choice("mode", &["clamp", "wrap", "mirror"])
                        .doc("Clamp, start again from min (sawtooth), or turn back (triangle)"),
                )
                .param(ParamSpec::bool("alpha", false).doc("Limit alpha too")),
            include_str!("limit.wgsl"),
        ),
        Builtin {
            // The lookup texture is read along a row by value, not shown.
            density: |_, _, input, _| if input == 1 { Density::Unrelated } else { Density::SAME },
            ..pass(
                NodeDesc::new("tex.lookup", "Colours from a lookup texture, indexed by the input's values")
                    .param(
                        ParamSpec::choice("index", &["luminance", "channels"])
                            .doc("One colour per pixel by luminance, or each channel through its own curve"),
                    )
                    .param(
                        ParamSpec::float("row", 0.5)
                            .range(0.0, 1.0)
                            .doc("Height in the lookup texture that is read; 0 is the top"),
                    )
                    .input("in", DataKind::Tex)
                    .input("lut", DataKind::Tex)
                    .output("out", DataKind::Tex),
                include_str!("lookup.wgsl"),
            )
        },
        pass(
            NodeDesc::new(
                "tex.matte",
                "Where the matte is white a shows, where it is black b; resolution of a",
            )
            .param(
                ParamSpec::choice("channel", &CHANNEL)
                    .doc("Channel of the matte that chooses; colour is read on black (premultiplied)"),
            )
            .param(ParamSpec::bool("invert", false).doc("Black shows a, white b"))
            .input("a", DataKind::Tex)
            .input("b", DataKind::Tex)
            .input("matte", DataKind::Tex)
            .output("out", DataKind::Tex),
            include_str!("matte.wgsl"),
        ),
        pass(
            filter("tex.monochrome", "Grey from luminance, average, maximum or one channel")
                .param(ParamSpec::choice(
                    "method",
                    &["luminance", "average", "maximum", "red", "green", "blue", "alpha"],
                ))
                .param(ParamSpec::bool("opaque", false).doc("Alpha 1 instead of the input's")),
            include_str!("monochrome.wgsl"),
        ),
        pass(
            filter("tex.hsv", "Turn the hue, scale saturation and value")
                .param(
                    ParamSpec::float("hue", 0.0)
                        .range(-180.0, 180.0)
                        .unit(Unit::Degrees)
                        .doc("Added to the hue: 120 turns red into green; any angle, it wraps"),
                )
                .param(ParamSpec::float("saturation", 1.0).range(0.0, 2.0).doc("Times the saturation"))
                .param(
                    ParamSpec::float("value", 1.0)
                        .range(0.0, 2.0)
                        .doc("Times the value (the brightest channel)"),
                ),
            include_str!("hsv.wgsl"),
        ),
        pass(
            NodeDesc::new("tex.remap", "Fetch the input where a map's red and green point")
                .param(ParamSpec::choice("extend", &EXTEND).doc("Outside the input"))
                .input("in", DataKind::Tex)
                .input("map", DataKind::Tex)
                .output("out", DataKind::Tex),
            include_str!("remap.wgsl"),
        ),
        Builtin {
            size: Size::Custom(|cx, (w, h)| {
                if cx.text("mode")? == "custom" {
                    return own_size(cx, (w, h));
                }
                let m = cx.float("multiply")?;
                let scale = |s: u32| -> Result<u32, CookError> {
                    let v = (f64::from(s) * m).round();
                    if (1.0..=MAX_SIDE).contains(&v) {
                        Ok(v as u32)
                    } else {
                        Err(CookError::failed(format!("{s} x {m} is not 1 to {MAX_SIDE} pixels")))
                    }
                };
                Ok((scale(w)?, scale(h)?))
            }),
            // A custom size is a choice of its own: the input needs what the output needs (the
            // whole frame becomes the whole frame). A multiple keeps its relation to the input,
            // so the input needs the output's need divided by it.
            density: |def, desc, _, _| {
                let custom = def.param(desc, "mode").and_then(|v| v.as_str()) == Some("custom");
                match float_of(def, desc, "multiply") {
                    Some(m) if !custom && m > 1e-6 => Density::needs([1.0 / m; 2]),
                    _ => Density::SAME,
                }
            },
            ..pass(
                size_params(
                    filter(
                        "tex.resolution",
                        "The input at another resolution; shrinking averages, so it does not alias",
                    )
                    .param(
                        ParamSpec::choice("mode", &["multiply", "custom"])
                            .doc("A multiple of the input's size, or width and height"),
                    )
                    .param(
                        ParamSpec::float("multiply", 0.5)
                            .clamp(0.001, 16.0)
                            .range(0.125, 4.0)
                            .curve(Curve::Log)
                            .doc("Of the input's width and height"),
                    ),
                    true,
                ),
                include_str!("resolution.wgsl"),
            )
        },
        pass(
            filter("tex.slope", "How steeply a channel rises: left to right in red, top to bottom in green")
                .param(ParamSpec::choice("channel", &CHANNEL))
                .param(ParamSpec::float("strength", 4.0).range(0.0, 50.0).curve(Curve::Power(2.0)))
                .param(ParamSpec::float("offset", 0.5).range(0.0, 1.0).doc("Value of a flat area")),
            include_str!("slope.wgsl"),
        ),
        pass(
            filter(
                "tex.threshold",
                "White where a channel is above a threshold, black (or transparent) elsewhere",
            )
            .param(ParamSpec::float("threshold", 0.5).range(0.0, 1.0))
            .param(
                ParamSpec::float("soft", 0.0).range(0.0, 0.5).doc("Width of a soft ramp above the threshold"),
            )
            .param(ParamSpec::choice("channel", &CHANNEL))
            .param(ParamSpec::bool("invert", false))
            .param(
                ParamSpec::choice("output", &["mask", "alpha"])
                    .doc("White on black, or the input where it passes and transparent elsewhere"),
            ),
            include_str!("threshold.wgsl"),
        ),
        Builtin {
            // Repeated twice, each copy is half the size: the input needs half the resolution.
            density: |def, desc, _, _| match def.param(desc, "repeat").and_then(|v| v.components()) {
                Some(&[x, y]) if x.abs() > 1e-6 && y.abs() > 1e-6 => {
                    Density::Linear([[x.abs(), 0.0], [0.0, y.abs()]])
                }
                _ => Density::SAME,
            },
            ..pass(
                filter("tex.tile", "The input repeated across and down, at its own resolution")
                    .param(
                        ParamSpec::vec2("repeat", [2.0, 2.0]).range(1.0, 16.0).doc("Copies across and down"),
                    )
                    .param(ParamSpec::bool("mirror", false).doc("Every other copy mirrored, so edges meet"))
                    .param(ParamSpec::vec2("offset", [0.0, 0.0]).range(-1.0, 1.0).more().doc("In tiles")),
                include_str!("tile.wgsl"),
            )
        },
    ]
}
