//! `tex.primitive`: one simple shape on a background, drawn from its signed distance, so the
//! edge stays smooth at any size and a border has an exact width in pixels.

use penumbra_core::{ParamSpec, Unit};
use penumbra_graph::NodeDesc;

use super::{Builtin, Size, same, sized};

/// Most corners of a polygon; the shader loops over them per pixel.
pub(crate) const MAX_SIDES: f64 = 64.0;

pub(super) fn builtin() -> Builtin {
    let shape = ParamSpec::choice("shape", &["rectangle", "ellipse", "polygon"])
        .doc("The shape drawn")
        .choice_doc("rectangle", "a rectangle, with rounded corners when corner is above 0")
        .choice_doc("ellipse", "an ellipse; with equal width and height a circle")
        .choice_doc("polygon", "a regular polygon with sides corners, one corner pointing up");
    Builtin {
        desc: sized(NodeDesc::new(
            "tex.primitive",
            "Draws one rectangle, ellipse or polygon with a fill and a border: masks, frames, simple graphics",
        ))
        .param(shape)
        .param(
            ParamSpec::vec2("center", [0.5; 2])
                .range(0.0, 1.0)
                .doc("Where the shape's centre sits, in image sizes: 0, 0 is the top left, 0.5, 0.5 the centre"),
        )
        .param(
            ParamSpec::vec2("size", [0.5; 2])
                .range(0.0, 2.0)
                .doc("Width and height of the shape in image heights: 1, 1 is a square as tall as the picture"),
        )
        .param(
            ParamSpec::float("rotate", 0.0)
                .range(-180.0, 180.0)
                .unit(Unit::Degrees)
                .doc("Turn around the centre, counter-clockwise on screen"),
        )
        .param(
            ParamSpec::int("sides", 6)
                .clamp(3.0, MAX_SIDES)
                .range(3.0, 12.0)
                .doc("Corners of the polygon. The corners lie on the ellipse that size describes")
                .applies_when("shape", &["polygon"]),
        )
        .param(
            ParamSpec::float("corner", 0.0)
                .clamp(0.0, 1.0)
                .range(0.0, 0.5)
                .doc("Radius of the rounded corners, in image heights; at most half the shorter side takes effect")
                .applies_when("shape", &["rectangle"]),
        )
        .param(ParamSpec::color("fill", [1.0; 4]).doc("Colour inside the shape, linear RGBA with straight alpha"))
        .param(
            ParamSpec::float("border", 0.0)
                .clamp(0.0, 10000.0)
                .range(0.0, 50.0)
                .unit(Unit::Pixels)
                .doc("Width of the border, in pixels, inside the shape's edge; 0 draws none"),
        )
        .param(ParamSpec::color("border_color", [0.0, 0.0, 0.0, 1.0]).doc("Colour of the border"))
        .param(
            ParamSpec::float("softness", 0.0)
                .clamp(0.0, 10000.0)
                .range(0.0, 100.0)
                .unit(Unit::Pixels)
                .doc("How many pixels the edges fade over; 0 gives a crisp edge, smoothed over one pixel"),
        )
        .param(
            ParamSpec::color("background", [0.0; 4])
                .doc("Colour around the shape; the default is transparent, for a composite over something else"),
        ),
        source: include_str!("primitive.wgsl"),
        size: Size::Params,
        animated: None,
        density: same,
        fold: false,
    }
}
