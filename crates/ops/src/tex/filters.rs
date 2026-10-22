//! Single-pass `tex` filters after TouchDesigner's TOPs of the same names: flip, channel mix,
//! chroma key, cross, displace, edge, fit, lens distort, limit, lookup, matte, monochrome, remap,
//! resolution, slope, threshold, tile. Each is a WGSL file in this directory with its params
//! declared here; behaviour is ours where TD's differs.

use penumbra_core::{Curve, ParamSpec, Unit, Value};
use penumbra_graph::{CookContext, CookError, DataKind, Density, NodeDef, NodeDesc};

use super::{Builtin, MAX_SIDE, Size, Sizes, extend, side};

/// Channels a filter can read one of.
const CHANNEL: [&str; 5] = ["luminance", "red", "green", "blue", "alpha"];

/// Channels of `tex.displace`'s map, in the order displace.wgsl numbers them.
const PUSH: [&str; 6] = ["red", "green", "blue", "alpha", "luminance", "none"];

/// One of `tex.displace`'s push channels, `default` chosen.
fn push(name: &str, default: &str, doc: &str) -> ParamSpec {
    let mut spec = ParamSpec::choice(name, &PUSH)
        .doc(doc)
        .choice_doc("red", "the map's red channel")
        .choice_doc("green", "the map's green channel")
        .choice_doc("blue", "the map's blue channel")
        .choice_doc("alpha", "the map's opacity")
        .choice_doc("luminance", "the map's brightness (Rec. 709)")
        .choice_doc("none", "no push this way");
    spec.default = Value::Text(default.to_string());
    spec
}

/// A `channel` param ([`CHANNEL`]): which value of the picture a filter reads.
pub(super) fn channel(doc: &str) -> ParamSpec {
    ParamSpec::choice("channel", &CHANNEL)
        .doc(doc)
        .choice_doc("luminance", "the brightness as the eye weighs it (Rec. 709)")
        .choice_doc("red", "the red channel only")
        .choice_doc("green", "the green channel only")
        .choice_doc("blue", "the blue channel only")
        .choice_doc("alpha", "the opacity")
}

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

/// `width`, `height` and `keep_size` of a node whose output size they set; `when` says when
/// they count (empty: always).
fn size_params(desc: NodeDesc, more: bool, when: &str) -> NodeDesc {
    let doc = |side: &str| {
        format!(
            "{side} of the output in pixels{when}; res.optimize may change it to what the projectors need, unless keep_size is on"
        )
    };
    let w = super::width_param().doc(&doc("Width"));
    let h = super::height_param().doc(&doc("Height"));
    let desc = if more { desc.param(w.more()).param(h.more()) } else { desc.param(w).param(h) };
    desc.param(super::keep_size())
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
                    .param(ParamSpec::bool("flip_x", false).doc("Mirror left to right: the left edge becomes the right"))
                    .param(ParamSpec::bool("flip_y", false).doc("Mirror top to bottom: the top edge becomes the bottom"))
                    .param(
                        ParamSpec::bool("flop", false)
                            .doc("Swap the axes (transpose); the output is as wide as the input is high"),
                    ),
                include_str!("flip.wgsl"),
            )
        },
        pass(
            filter("tex.channel_mix", "Each output channel a weighted sum of the input's channels")
                .param(
                    ParamSpec::vec4("red", [1.0, 0.0, 0.0, 0.0])
                        .range(-2.0, 2.0)
                        .doc("How much of the input's r, g, b and a makes the output's red"),
                )
                .param(
                    ParamSpec::vec4("green", [0.0, 1.0, 0.0, 0.0])
                        .range(-2.0, 2.0)
                        .doc("How much of the input's r, g, b and a makes the output's green"),
                )
                .param(
                    ParamSpec::vec4("blue", [0.0, 0.0, 1.0, 0.0])
                        .range(-2.0, 2.0)
                        .doc("How much of the input's r, g, b and a makes the output's blue"),
                )
                .param(
                    ParamSpec::vec4("alpha", [0.0, 0.0, 0.0, 1.0])
                        .range(-2.0, 2.0)
                        .doc("How much of the input's r, g, b and a makes the output's alpha"),
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
            filter("tex.chroma_key", "Make pixels near a key colour transparent (green or blue screen)")
                .param(
                    ParamSpec::color("key", [0.0, 1.0, 0.0, 1.0])
                        .doc("The screen's colour; only its hue and saturation count, so shadows on the screen key too"),
                )
                .param(
                    ParamSpec::float("tolerance", 0.1)
                        .range(0.0, 0.5)
                        .doc("Chroma distance from the key that is fully transparent"),
                )
                .param(
                    ParamSpec::float("softness", 0.08)
                        .range(0.0, 0.5)
                        .doc("Width of the partly transparent edge beyond tolerance, in the same chroma distance"),
                )
                .param(
                    ParamSpec::float("spill", 0.5)
                        .range(0.0, 1.0)
                        .doc("How much of the key's tint is removed from what stays"),
                )
                .param(ParamSpec::bool("invert", false).doc("Keep the key colour, drop the rest"))
                .param(
                    ParamSpec::choice("output", &["keyed", "matte"])
                        .doc("What the node puts out")
                        .choice_doc("keyed", "the input with the key colour transparent and its spill removed")
                        .choice_doc("matte", "the alpha it would get, as white (kept) on black (keyed out)"),
                ),
            include_str!("chroma_key.wgsl"),
        ),
        pass(
            NodeDesc::new("tex.cross", "Mix of two inputs; resolution of a")
                .param(
                    ParamSpec::float("cross", 0.5)
                        .range(0.0, 1.0)
                        .doc("Mix between the inputs: 0 is all a, 1 all b, 0.5 half each"),
                )
                .input("a", DataKind::Tex)
                .input("b", DataKind::Tex)
                .output("out", DataKind::Tex),
            include_str!("cross.wgsl"),
        ),
        pass(
            NodeDesc::new("tex.displace", "Push each pixel of the input by the values of a map")
                .param(push(
                    "horizontal",
                    "red",
                    "Channel of the map that shifts the picture across, by weight x per unit away from midpoint",
                ))
                .param(push(
                    "vertical",
                    "green",
                    "Channel of the map that shifts the picture up and down, by weight y per unit away from midpoint",
                ))
                .param(
                    ParamSpec::vec2("weight", [0.1, 0.1])
                        .range(-1.0, 1.0)
                        .doc("Push for a map value 1 above the midpoint, in image sizes"),
                )
                .param(ParamSpec::float("midpoint", 0.5).range(0.0, 1.0).doc("Map value that does not push"))
                .param(extend("hold").more())
                .input("in", DataKind::Tex)
                .input("map", DataKind::Tex)
                .output("out", DataKind::Tex),
            include_str!("displace.wgsl"),
        ),
        pass(
            filter("tex.edge", "Edges of the luminance (Sobel)")
                .param(
                    ParamSpec::float("strength", 2.0)
                        .range(0.0, 10.0)
                        .doc("Gain on the edge: higher makes faint edges visible and strong ones solid (clipped at 1)"),
                )
                .param(
                    ParamSpec::float("distance", 1.0)
                        .range(0.5, 8.0)
                        .unit(Unit::Pixels)
                        .doc("Distance of the neighbours compared; larger finds broader, softer edges"),
                )
                .param(ParamSpec::color("color", [1.0; 4]).doc("Colour of the edges"))
                .param(
                    ParamSpec::choice("mode", &["edges", "alpha", "over"])
                        .doc("What the edges are drawn on")
                        .choice_doc("edges", "on opaque black")
                        .choice_doc("alpha", "on transparent, to lay over something else")
                        .choice_doc("over", "over the input"),
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
                    "",
                )
                .param(
                    ParamSpec::choice("fit", &["fit", "fill", "stretch", "native"])
                        .doc("How the input sits in the frame")
                        .choice_doc("fit", "whole and as large as fits, aspect kept; bars where it does not fill")
                        .choice_doc("fill", "covering the frame, aspect kept; what sticks out is cropped")
                        .choice_doc("stretch", "covering the frame, distorted to its aspect")
                        .choice_doc("native", "pixel for pixel, unscaled"),
                )
                .param(
                    ParamSpec::vec2("justify", [0.5, 0.5])
                        .range(0.0, 1.0)
                        .doc("Where the image sits in the frame: 0 left or top, 1 right or bottom"),
                )
                .param(
                    ParamSpec::choice("extend", &["zero", "hold", "repeat", "mirror"])
                        .doc("What fills the frame where the input does not reach (the bars of fit or native)")
                        .choice_doc("zero", "transparent black")
                        .choice_doc("hold", "the input's edge pixels continue outwards")
                        .choice_doc("repeat", "the input again, tiled")
                        .choice_doc("mirror", "the input again, mirrored, so the copies meet at the edges"),
                ),
                include_str!("fit.wgsl"),
            )
        },
        pass(
            filter("tex.lens_distort", "Radial lens distortion (barrel or pincushion) around a centre")
                .param(
                    ParamSpec::float("k1", 0.1)
                        .range(-1.0, 1.0)
                        .doc("Main term; positive pulls the edges in (barrel), negative pushes them out"),
                )
                .param(
                    ParamSpec::float("k2", 0.0).range(-1.0, 1.0).doc("Fourth-order term, for the far edges"),
                )
                .param(
                    ParamSpec::float("scale", 1.0)
                        .range(0.25, 4.0)
                        .curve(Curve::Log)
                        .doc("Zoom after the distortion: above 1 enlarges, to hide the edges barrel distortion pulls in"),
                )
                .param(
                    ParamSpec::vec2("center", [0.5, 0.5])
                        .range(0.0, 1.0)
                        .more()
                        .doc("Optical centre; 0, 0 is the top left"),
                )
                .param(extend("zero")),
            include_str!("lens_distort.wgsl"),
        ),
        pass(
            filter("tex.limit", "Keep values within a range: clamp, wrap around or mirror")
                .param(
                    ParamSpec::float("min", 0.0)
                        .range(-1.0, 2.0)
                        .doc("Lowest value kept, per channel of the straight colour"),
                )
                .param(
                    ParamSpec::float("max", 1.0)
                        .range(-1.0, 2.0)
                        .doc("Highest value kept; at or below min everything becomes min"),
                )
                .param(
                    ParamSpec::choice("mode", &["clamp", "wrap", "mirror"])
                        .doc("What happens to values outside min to max")
                        .choice_doc("clamp", "they stop at min or max")
                        .choice_doc("wrap", "they start again from min: a ramp becomes a sawtooth")
                        .choice_doc("mirror", "they turn back: a ramp becomes a triangle wave"),
                )
                .param(
                    ParamSpec::bool("alpha", false)
                        .doc("Limit alpha the same way; off keeps the input's alpha"),
                ),
            include_str!("limit.wgsl"),
        ),
        Builtin {
            // The lookup texture is read along a row by value, not shown.
            density: |_, _, input, _| if input == 1 { Density::Unrelated } else { Density::SAME },
            ..pass(
                NodeDesc::new("tex.lookup", "Colours from a lookup texture, indexed by the input's values")
                    .param(
                        ParamSpec::choice("index", &["luminance", "channels"])
                            .doc("Which input values pick the place in the lookup row (0 its left end, 1 its right)")
                            .choice_doc("luminance", "the brightness picks one colour of the row (a gradient map)")
                            .choice_doc("channels", "red, green and blue each through the row's own channel (a curve per channel)"),
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
                channel("Channel of the matte that chooses; colour is read on black (premultiplied)"),
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
                .param(
                    ParamSpec::choice("method", &["luminance", "average", "maximum", "red", "green", "blue", "alpha"])
                        .doc("How the grey is made from the colour")
                        .choice_doc("luminance", "brightness as the eye weighs it (Rec. 709): green counts most, blue least")
                        .choice_doc("average", "red, green and blue equally")
                        .choice_doc("maximum", "the brightest of red, green and blue")
                        .choice_doc("red", "the red channel only")
                        .choice_doc("green", "the green channel only")
                        .choice_doc("blue", "the blue channel only")
                        .choice_doc("alpha", "the opacity, as grey"),
                )
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
                .param(
                    ParamSpec::float("saturation", 1.0)
                        .range(0.0, 2.0)
                        .doc("Multiplies the saturation: 0 is grey, 1 unchanged, 2 more vivid (clipped at full)"),
                )
                .param(
                    ParamSpec::float("value", 1.0)
                        .range(0.0, 2.0)
                        .doc("Multiplies the value (the brightest channel): 0 is black, 2 twice as bright"),
                ),
            include_str!("hsv.wgsl"),
        ),
        pass(
            NodeDesc::new("tex.remap", "Fetch the input where a map's red and green point")
                .param(extend("hold").doc("What shows where the map points outside 0 to 1"))
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
                            .doc("How the output size is set")
                            .choice_doc("multiply", "the input's size times multiply")
                            .choice_doc("custom", "width and height (under more), whatever the input"),
                    )
                    .param(
                        ParamSpec::float("multiply", 0.5)
                            .clamp(0.001, 16.0)
                            .range(0.125, 4.0)
                            .curve(Curve::Log)
                            .doc("Factor on the input's width and height (mode multiply): 0.5 halves both"),
                    ),
                    true,
                    " (mode custom)",
                ),
                include_str!("resolution.wgsl"),
            )
        },
        pass(
            filter("tex.slope", "How steeply a channel rises: left to right in red, top to bottom in green")
                .param(channel("Channel whose slope is measured"))
                .param(
                    ParamSpec::float("strength", 4.0)
                        .range(0.0, 50.0)
                        .curve(Curve::Power(2.0))
                        .doc("Gain on the slope (value change per pixel); higher makes gentle slopes visible"),
                )
                .param(
                    ParamSpec::float("offset", 0.5)
                        .range(0.0, 1.0)
                        .doc("Value of a flat area; rising slopes lie above it, falling below. 0.5 suits tex.displace's midpoint"),
                ),
            include_str!("slope.wgsl"),
        ),
        pass(
            filter(
                "tex.threshold",
                "White where a channel is above a threshold, black (or transparent) elsewhere",
            )
            .param(
                ParamSpec::float("threshold", 0.5)
                    .range(0.0, 1.0)
                    .doc("Value of the channel from which a pixel passes (on straight colour)"),
            )
            .param(
                ParamSpec::float("soft", 0.0).range(0.0, 0.5).doc("Width of a soft ramp above the threshold"),
            )
            .param(channel("Channel compared with the threshold"))
            .param(ParamSpec::bool("invert", false).doc("Pass what lies below the threshold instead"))
            .param(
                ParamSpec::choice("output", &["mask", "alpha"])
                    .doc("What the node puts out")
                    .choice_doc("mask", "white where a pixel passes, black elsewhere")
                    .choice_doc("alpha", "the input where it passes, transparent elsewhere"),
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
                        ParamSpec::vec2("repeat", [2.0, 2.0])
                            .range(1.0, 16.0)
                            .doc("Copies across and down, each shrunk to fit; fractions show part of a copy"),
                    )
                    .param(ParamSpec::bool("mirror", false).doc("Every other copy mirrored, so edges meet"))
                    .param(
                        ParamSpec::vec2("offset", [0.0, 0.0])
                            .range(-1.0, 1.0)
                            .more()
                            .doc("Shifts the pattern, in tiles: 0.5 moves it by half a copy"),
                    ),
                include_str!("tile.wgsl"),
            )
        },
    ]
}
