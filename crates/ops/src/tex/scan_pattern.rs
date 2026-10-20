//! `tex.scan_pattern`: the pictures a projector shows for a structured light scan, one at a time
//! by index, so a scan can step through them by setting one param. The order and the stripes
//! are those of `penumbra_calib::scan::Patterns`, which decodes what a camera saw of them.

use penumbra_core::ParamSpec;
use penumbra_graph::NodeDesc;

use super::{Builtin, Size, same, sized};

pub(super) fn builtin() -> Builtin {
    Builtin {
        desc: sized(NodeDesc::new(
            "tex.scan_pattern",
            "Structured light: Gray-code stripes by index (white, black, then columns and rows, each bit with its inverse); 46 pictures at 1920 x 1080",
        ))
        .param(
            ParamSpec::int("pattern", 0)
                .clamp(0.0, 1000.0)
                .range(0.0, 45.0)
                .doc("Which picture: 0 white, 1 black, then a column bit (coarsest first) and its inverse in turn, then the row bits; black past the last (scan.patterns lists them)"),
        )
        .param(
            ParamSpec::float("level", 1.0)
                .range(0.0, 1.0)
                .more()
                .doc("Brightness of the lit stripes; lower it only if the camera clips even with its exposure turned down"),
        ),
        source: include_str!("scan_pattern.wgsl"),
        size: Size::Params,
        animated: None,
        density: same,
        fold: false,
    }
}
