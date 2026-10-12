//! `calib.*`: the calibration math of crate `calib` as commands. Pure functions of their params: no
//! project, no engine state. The calibration mode of the UI stores its pairs and results in the
//! project and calls the same functions.

use penumbra_calib::{Camera, Homography, Options, calibrate};
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, FnCommand, Registry, args};
use crate::commands::to_json;

type Result<T> = std::result::Result<T, CommandError>;

/// Registers the `calib.*` commands into `registry`.
pub fn register(registry: &mut Registry) {
    registry.register(FnCommand::new(
        "calib.solve",
        "Calibrate a projector from world and picture points; {pairs: [[[x,y,z],[u,v]],...], size: [w,h], initial?, options?}",
        calib_solve,
    ));
    registry.register(FnCommand::new(
        "calib.project",
        "Pixels of world points through a calibrated camera (null behind it); {camera, points: [[x,y,z],...]}",
        calib_project,
    ));
    registry.register(FnCommand::new(
        "calib.homography",
        "Homography from 4 or more point pairs (normalised DLT); {pairs: [[[x,y],[u,v]],...]}",
        calib_homography,
    ));
}

fn err(e: impl std::fmt::Display) -> CommandError {
    CommandError(e.to_string())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SolveArgs {
    pairs: Vec<([f64; 3], [f64; 2])>,
    size: [u32; 2],
    initial: Option<Camera>,
    #[serde(default)]
    options: Options,
}

fn calib_solve(p: &Json) -> Result<Json> {
    let a: SolveArgs = args(p)?;
    let s = calibrate(&a.pairs, a.size, a.initial.as_ref(), &a.options).map_err(err)?;
    to_json(&s)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectArgs {
    camera: Camera,
    points: Vec<[f64; 3]>,
}

fn calib_project(p: &Json) -> Result<Json> {
    let a: ProjectArgs = args(p)?;
    let pixels: Vec<Option<[f64; 2]>> = a.points.iter().map(|w| a.camera.project(*w)).collect();
    Ok(json!({"pixels": pixels, "position": a.camera.position()}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HomographyArgs {
    pairs: Vec<([f64; 2], [f64; 2])>,
}

fn calib_homography(p: &Json) -> Result<Json> {
    let a: HomographyArgs = args(p)?;
    let h = Homography::from_pairs(&a.pairs).map_err(err)?;
    Ok(json!({"rows": h.rows(), "rms": h.rms_error(&a.pairs)}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> Registry {
        let mut r = Registry::new();
        register(&mut r);
        r
    }

    #[test]
    fn solve_and_project_round_trip() {
        let r = registry();
        // A camera at (0, 0, -5) looking down +z with f = 2000 and the centre at (960, 540).
        let camera = json!({
            "intrinsics": {"size": [1920, 1080], "focal": [2000.0, 2000.0], "principal": [960.0, 540.0]},
            "pose": {"rotation": [0.0, 0.0, 0.0], "translation": [0.0, 0.0, 5.0]},
        });
        let points = json!([
            [0, 0, 0],
            [1, 0, 0],
            [0, 1, 0],
            [0, 0, 1],
            [1, 1, 0.5],
            [-1, 0.5, 0.3],
            [0.4, -0.7, -0.6]
        ]);
        let out = r.dispatch("calib.project", &json!({"camera": camera, "points": points}));
        assert_eq!(out["ok"], true, "{out}");
        assert_eq!(out["result"]["pixels"][0], json!([960.0, 540.0]));
        assert_eq!(out["result"]["position"], json!([0.0, 0.0, -5.0]));
        let pixels = out["result"]["pixels"].as_array().unwrap().clone();
        let pairs: Vec<Json> =
            points.as_array().unwrap().iter().zip(&pixels).map(|(w, q)| json!([w, q])).collect();
        let out = r.dispatch("calib.solve", &json!({"pairs": pairs, "size": [1920, 1080]}));
        assert_eq!(out["ok"], true, "{out}");
        let res = &out["result"];
        assert!(res["rms"].as_f64().unwrap() < 1e-6, "{res}");
        assert!((res["camera"]["intrinsics"]["focal"][0].as_f64().unwrap() - 2000.0).abs() < 1e-4, "{res}");
        assert_eq!(res["start"], "dlt");
        assert_eq!(res["errors"].as_array().unwrap().len(), 7);
        // Options and an initial camera are accepted; unknown fields and bad input are not.
        let out = r.dispatch(
            "calib.solve",
            &json!({"pairs": pairs, "size": [1920, 1080], "initial": camera, "options": {"fix_focal": true}}),
        );
        assert_eq!(out["ok"], true, "{out}");
        let out =
            r.dispatch("calib.solve", &json!({"pairs": pairs, "size": [1920, 1080], "options": {"fix": 1}}));
        assert_eq!(out["ok"], false);
        let out = r.dispatch("calib.solve", &json!({"pairs": [], "size": [1920, 1080]}));
        assert_eq!(out["ok"], false);
        assert!(out["error"].as_str().unwrap().contains("at least 4"), "{out}");
    }

    #[test]
    fn homography_from_pairs() {
        let r = registry();
        let out = r.dispatch(
            "calib.homography",
            &json!({"pairs": [[[0, 0], [10, 10]], [[1, 0], [30, 10]], [[1, 1], [30, 30]], [[0, 1], [10, 30]]]}),
        );
        assert_eq!(out["ok"], true, "{out}");
        assert!((out["result"]["rows"][0][0].as_f64().unwrap() - 20.0).abs() < 1e-9, "{out}");
        let out = r.dispatch("calib.homography", &json!({"pairs": [[[0, 0], [1, 1]]]}));
        assert_eq!(out["ok"], false);
    }
}
