//! `tex.scan_pattern`: the pictures a projector shows for a structured light scan, one at a time
//! by index, so a scan can step through them by setting one param. The order and the stripes
//! are those of `penumbra_calib::scan::Patterns`, which decodes what a camera saw of them.

use penumbra_core::ParamSpec;
use penumbra_graph::NodeDesc;

use super::{Builtin, Size, same, sized_keeping};

pub(super) fn builtin() -> Builtin {
    Builtin {
        // keep_size on: the stripes must match the projector pixel for pixel, so res.optimize
        // must not resize them.
        desc: sized_keeping(
            NodeDesc::new(
                "tex.scan_pattern",
                "The striped pictures of a structured light scan (Gray code), one per `pattern`: for scanning with another camera by hand",
            ),
            true,
        )
        .param(
            ParamSpec::int("pattern", 0)
                .clamp(0.0, 1000.0)
                .range(0.0, 45.0)
                .doc("Which picture: 0 white, 1 black, then each column bit (coarsest first) followed by its inverse, then the row bits the same way. Past the last: black. scan.patterns lists them"),
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

#[cfg(test)]
mod tests {
    use penumbra_core::Value;

    #[test]
    fn a_scan_pattern_keeps_its_size() {
        let b = super::builtin();
        let keep = b.desc.param_spec(super::super::KEEP_SIZE).map(|p| p.default.clone());
        assert_eq!(keep, Some(Value::Bool(true)));
        let width = b.desc.param_spec("width").map(|p| p.default.clone());
        assert!(width.is_some());
    }
}
