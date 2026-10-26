//! `tex.face_content`: a picture or video delivered for one face of a scanned scene, put onto
//! that face. `scan.face_template` writes, per face, a folder with the face's template for the
//! designer and its uv maps (where each camera or projector pixel lies on the face); this node
//! reads the map of the face its `face` param names and stretches its input over it. Out comes
//! the picture in the camera's frame, to compose with other content made there, or straight in
//! one projector's pixels.
//!
//! The map is decoded off the cook thread once, and again only when it changes on disk (the
//! template made again after a rescan). Per frame it is one pass.

use std::sync::Arc;

use penumbra_core::{Error, ParamSpec};
use penumbra_gpu::{Gpu, Pass, PassInput, Tex, TexRing};
use penumbra_graph::{
    AsyncSlot, CookContext, CookError, Cooked, Data, DataKind, FileWatch, FnNodeType, JobStatus, NodeDesc,
    NodeTypes, Operator,
};

use super::{PassCell, gpu, waiting};
use crate::on_gpu;

pub const FACE_CONTENT: &str = "tex.face_content";
const SOURCE: &str = include_str!("face_content.wgsl");

/// Where the scene is unless the node says otherwise (`scan.scene`'s default folder).
pub const DEFAULT_SCENE: &str = "scene";
/// The folder inside a scene folder that holds one folder per face (`scan.face_template`).
pub const FACES_FOLDER: &str = "faces";
/// A face's uv in the camera's picture, inside its folder.
pub const UV_CAMERA: &str = "uv_camera.exr";

/// A face's uv in projector `n`'s pixels, inside its folder.
pub fn uv_projector(n: usize) -> String {
    format!("uv_{n}.exr")
}

/// How a face is named in its folder's name: lower case letters and digits, anything else one
/// `_`, none at either end.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}

/// What a face name points at: a face of `scan.scene` by its number, or a region of
/// `scan.regions` by its number or its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FaceRef {
    Face(u16),
    Region(u16),
    Named(String),
}

impl FaceRef {
    /// `7` or `face 7` a face, `region 3` a region by number, anything else a region's name.
    /// `None` for an empty name.
    pub fn parse(text: &str) -> Option<FaceRef> {
        let t = text.trim();
        if t.is_empty() {
            return None;
        }
        let lower = t.to_lowercase();
        let number = |rest: &str| rest.trim().parse::<u16>().ok().filter(|n| *n > 0);
        if let Some(n) = number(&lower) {
            return Some(FaceRef::Face(n));
        }
        if let Some(n) = lower.strip_prefix("face").and_then(number) {
            return Some(FaceRef::Face(n));
        }
        if let Some(n) = lower.strip_prefix("region").and_then(number) {
            return Some(FaceRef::Region(n));
        }
        Some(FaceRef::Named(t.to_string()))
    }

    /// The face's folder inside the scene's `faces` folder.
    pub fn folder(&self) -> String {
        match self {
            FaceRef::Face(n) => format!("face_{n}"),
            FaceRef::Region(n) => format!("region_{n}"),
            FaceRef::Named(name) => match slug(name).as_str() {
                "" => "region".to_string(),
                s => format!("region_{s}"),
            },
        }
    }
}

fn desc() -> NodeDesc {
    NodeDesc::new(
        FACE_CONTENT,
        "Puts a picture or video delivered for one face of a scanned scene onto that face, through the scan's mapping",
    )
    .block(penumbra_graph::Block::ScanAndShapes)
    .param(ParamSpec::text("face", "").doc(
        "The face: a region's name, `region 3`, or `face 7` (scan.faces lists them). The menu beside it \
         picks one and makes its template when there is none yet",
    ))
    .param(ParamSpec::text("scene", DEFAULT_SCENE).doc(
        "The folder scan.scene wrote, relative to the project. The faces' templates are in its faces folder",
    ))
    .param(
        ParamSpec::int("projector", -1)
            .clamp(-1.0, 15.0)
            .range(-1.0, 3.0)
            .special(-1.0, "camera")
            .doc(
                "Where the picture is made: camera, the camera's picture, to compose with other content \
                 made there; 0, 1, ...: that projector's own pixels (the order of scan.scene's scans), \
                 sharper than the camera's picture remapped",
            ),
    )
    .param(
        ParamSpec::choice("fit", &["stretch", "contain", "cover"])
            .doc("How the picture fills the face when its aspect differs from the face's")
            .choice_doc("stretch", "fills the face exactly, distorted if the aspect differs")
            .choice_doc("contain", "whole and undistorted, with empty bands at two sides")
            .choice_doc("cover", "fills the face undistorted, cut off at two sides"),
    )
    .param(ParamSpec::bool("objects", false).doc(
        "Also over objects standing in front of the face (a pumpkin on a lid). Off: on the face only",
    ))
    .param(ParamSpec::float("aspect", 0.0).clamp(0.0, 100.0).more().special(0.0, "template").doc(
        "The face's width over its height, for contain and cover: template, as the face's template says",
    ))
    .input("in", DataKind::Tex)
    .output("out", DataKind::Tex)
    .describe_input("in", "The picture delivered for the face, at any size; best in its template's aspect")
    .describe_output(
        "out",
        "The picture on the face, transparent elsewhere; the camera's size or the projector's",
    )
}

fn pass(gpu: &Gpu) -> Result<Arc<Pass>, Error> {
    gpu.cached_pass(FACE_CONTENT, SOURCE, &desc().params)
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(
        FnNodeType::new(desc(), || Box::new(FaceContent::default()) as Box<dyn Operator>)
            .warm_up(on_gpu(|gpu| pass(gpu).map(|_| ()))),
    )
}

/// The face's template (`face.json`): only the aspect is read here.
fn template_aspect(file: &std::path::Path) -> Option<f64> {
    let text = std::fs::read_to_string(file).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let a = json.get("aspect")?;
    let (w, h) = (a.get(0)?.as_f64()?, a.get(1)?.as_f64()?);
    let r = w / h;
    (r.is_finite() && r > 0.0).then_some(r)
}

#[derive(Default)]
struct FaceContent {
    /// The uv map and the template's aspect.
    slot: AsyncSlot<(Tex, Option<f64>)>,
    watch: Option<FileWatch>,
    pass: PassCell,
    ring: TexRing,
}

impl Operator for FaceContent {
    fn trim(&mut self) {
        self.ring.trim();
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        self.slot.poll();
        let gpu = gpu(cx)?;
        let face_text = cx.text("face")?;
        let face = FaceRef::parse(face_text).ok_or_else(|| {
            CookError::failed("pick a face: the menu beside `face` lists the scene's faces")
        })?;
        let scene = cx.text("scene")?;
        let projector = cx.int("projector")?;
        let file_name = match usize::try_from(projector) {
            Ok(n) => uv_projector(n),
            Err(_) => UV_CAMERA.to_string(),
        };
        let folder = cx.resolve_path(scene)?.join(FACES_FOLDER).join(face.folder());
        let path = folder.join(&file_name);
        let moved = self.watch.as_ref().is_none_or(|w| w.path() != path);
        if moved {
            self.watch = Some(cx.watch_file(&path));
        }
        if !path.exists() {
            return Err(CookError::failed(format!(
                "no template for `{}` in {}/{FACES_FOLDER} yet ({file_name} missing): pick the face from the \
                 menu beside `face`, or run scan.face_template",
                face_text.trim(),
                scene
            )));
        }
        let changed = self.watch.as_ref().is_some_and(FileWatch::take_changed);
        if moved || changed {
            let gpu = gpu.clone();
            let label = path.display().to_string();
            let p = path.clone();
            let json = folder.join("face.json");
            self.slot.start(cx.spawn(move |_job| {
                let decoded =
                    penumbra_gpu::image::decode_file(&p, gpu.max_texture_size()).map_err(|e| e.0)?;
                let tex = gpu.upload(&decoded, &label).map_err(|e| e.0)?;
                Ok((tex, template_aspect(&json)))
            }));
        }
        if let Some(e) = self.slot.error() {
            return Err(CookError::failed(e));
        }
        let Some((uv, template)) = self.slot.latest().cloned() else { return Err(CookError::Pending) };
        let content = cx.input_as::<Tex>(0).ok_or_else(|| waiting(cx))?;
        let pass = self.pass.get(gpu, pass)?;
        let (w, h) = uv.size();
        let target = self.ring.next(gpu, w, h, FACE_CONTENT)?;
        let asked = cx.float("aspect")?;
        let aspect = if asked > 0.0 { asked } else { template.unwrap_or(1.0) };
        let param = |name: &str| match name {
            "aspect" => Some(penumbra_core::Value::Float(aspect)),
            _ => cx.param(name).cloned(),
        };
        let inputs = [Some(content), Some(&uv)];
        pass.render(gpu, &target, &PassInput { inputs: &inputs, seconds: 0.0, frame: 0, param: &param })?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, target)]))
    }

    fn job_status(&self) -> Option<JobStatus> {
        Some(self.slot.status())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn face_names_lead_to_their_folders() {
        assert_eq!(FaceRef::parse("7"), Some(FaceRef::Face(7)));
        assert_eq!(FaceRef::parse(" Face 12 "), Some(FaceRef::Face(12)));
        assert_eq!(FaceRef::parse("region 3"), Some(FaceRef::Region(3)));
        assert_eq!(FaceRef::parse(""), None);
        assert_eq!(FaceRef::parse("0"), Some(FaceRef::Named("0".into())));
        assert_eq!(FaceRef::parse("7").map(|f| f.folder()), Some("face_7".into()));
        assert_eq!(FaceRef::parse("region 3").map(|f| f.folder()), Some("region_3".into()));
        assert_eq!(
            FaceRef::parse("Left wall, upper crate front").map(|f| f.folder()),
            Some("region_left_wall_upper_crate_front".into())
        );
        assert_eq!(slug("  -- Ä b--c "), "b_c");
        assert_eq!(FaceRef::parse("?!").map(|f| f.folder()), Some("region".into()));
    }
}
