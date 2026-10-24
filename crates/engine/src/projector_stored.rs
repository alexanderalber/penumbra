//! `projector.match` from stored measurements: per projector one file of what the camera read
//! of it (written by a camera run with `save`), combined with any others into a correction
//! without the camera ([`penumbra_calib::colour::stored`]).
//!
//! A measurement belongs to the device, not to the project: the file names the node it was
//! taken through only for reference, and `from` maps the current project's nodes to files.
//! Readings of different projectors are comparable only when they were taken with the same
//! camera, exposure and white balance on the same patch of wall; files from one run are, files
//! from different runs are combined with a warning.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use penumbra_calib::colour::stored::{self, Stored};
use penumbra_calib::colour::{self, Measured};
use penumbra_core::{Name, NodeId, NodePath, ParamPath, Value};
use penumbra_graph::Op;
use penumbra_ops::out::correct;
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use crate::command::CommandError;
use crate::commands::{edit_in, outcome};
use crate::engine::Engine;
use crate::projector::{MatchArgs, projector_list};

type CmdResult<T> = std::result::Result<T, CommandError>;

/// What a measurement file says it is.
pub const FORMAT: &str = "penumbra projector measurement";
pub const VERSION: u32 = 1;
/// Largest measurement file read.
const MAX_BYTES: u64 = 1 << 20;
/// Most ramp levels a file may hold.
const MAX_LEVELS: usize = 64;

/// The camera a measurement was taken with.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CameraOf {
    #[serde(default)]
    pub node: String,
    #[serde(default)]
    pub device: String,
    #[serde(default)]
    pub mode: String,
}

/// One projector's measurement as stored: what the camera read in its spot, black subtracted
/// (`black` itself as read).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MeasurementFile {
    pub format: String,
    pub version: u32,
    /// The node it was measured through.
    #[serde(default)]
    pub projector: String,
    /// That node's `display` param then.
    #[serde(default)]
    pub display: String,
    /// Free text: the projector's settings, lamp hours, where it stood.
    #[serde(default)]
    pub note: String,
    /// Shared by every file of one run: their readings are comparable.
    #[serde(default)]
    pub run: String,
    /// Seconds since 1970 when measured.
    #[serde(default)]
    pub unix_time: f64,
    #[serde(default)]
    pub camera: CameraOf,
    #[serde(default)]
    pub spot_pixels: usize,
    /// Measured in the spot every projector of the run lights.
    #[serde(default)]
    pub shared_spot: bool,
    pub levels: Vec<f64>,
    #[serde(default)]
    pub black: [f64; 3],
    #[serde(default)]
    pub white: Option<[f64; 3]>,
    pub ramps: [Vec<[f64; 3]>; 3],
    /// Grey (every channel at the level) per level; older measurements have none.
    #[serde(default)]
    pub greys: Option<Vec<[f64; 3]>>,
}

impl MeasurementFile {
    fn stored(&self) -> Stored {
        Stored {
            measured: Measured { levels: self.levels.clone(), ramps: self.ramps.clone() },
            greys: self.greys.clone(),
        }
    }
}

/// `from`: a folder with a `<node name>.json` per projector, or a file per projector path.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Source {
    Folder(String),
    Files(BTreeMap<String, String>),
}

/// What a camera run saves of one projector, besides the readings.
pub(crate) struct RunInfo<'a> {
    pub run: &'a str,
    pub unix_time: f64,
    pub camera: CameraOf,
    pub shared_spot: bool,
    pub note: &'a str,
}

/// An id for the files of one run, and its time.
pub(crate) fn run_id() -> (String, f64) {
    let t = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
    (format!("{}", (t * 1000.0) as u64), t)
}

/// The file name of the projector at `path` in a folder: its node name.
pub(crate) fn file_name(path: &str) -> CmdResult<String> {
    let node = NodePath::parse(path)?;
    let name = node.name().map(|n| n.to_string()).ok_or_else(|| format!("{path} has no name"))?;
    Ok(format!("{name}.json"))
}

/// Writes one measurement file per projector into `dir` (created if missing); the paths
/// written.
pub(crate) fn save(dir: &Path, files: &[(String, MeasurementFile)]) -> CmdResult<Vec<String>> {
    std::fs::create_dir_all(dir).map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
    let mut names: Vec<String> = Vec::with_capacity(files.len());
    for (path, _) in files {
        let name = file_name(path)?;
        if names.contains(&name) {
            return Err(
                format!("two projectors would both be saved as {name}; save them in separate runs").into()
            );
        }
        names.push(name);
    }
    let mut written = Vec::with_capacity(files.len());
    for ((_, file), name) in files.iter().zip(names) {
        let target = dir.join(&name);
        let text = serde_json::to_string_pretty(file).map_err(|err| format!("cannot write {name}: {err}"))?;
        let part = dir.join(format!("{name}.part"));
        std::fs::write(&part, text).map_err(|err| format!("cannot write {}: {err}", part.display()))?;
        std::fs::rename(&part, &target).map_err(|err| format!("cannot write {}: {err}", target.display()))?;
        written.push(target.display().to_string());
    }
    Ok(written)
}

/// Reads and checks a measurement file.
pub fn read(file: &Path) -> CmdResult<MeasurementFile> {
    let name = file.display();
    let size = std::fs::metadata(file).map_err(|err| format!("cannot read {name}: {err}"))?.len();
    if size > MAX_BYTES {
        return Err(format!("{name} is larger than {} KiB; not a measurement", MAX_BYTES >> 10).into());
    }
    let text = std::fs::read_to_string(file).map_err(|err| format!("cannot read {name}: {err}"))?;
    let m: MeasurementFile =
        serde_json::from_str(&text).map_err(|err| format!("{name} is not a projector measurement: {err}"))?;
    if m.format != FORMAT {
        return Err(format!("{name} is not a projector measurement (format `{}`)", m.format).into());
    }
    if m.version > VERSION {
        return Err(format!(
            "{name} is of a newer version ({}) than this penumbra reads ({VERSION})",
            m.version
        )
        .into());
    }
    if m.levels.len() > MAX_LEVELS {
        return Err(format!("{name} has more than {MAX_LEVELS} levels").into());
    }
    Ok(m)
}

/// The files `from` names for `projectors` (node, path), resolved against the project folder
/// `dir`.
fn files_of(dir: &Path, from: &Source, projectors: &[(NodeId, String)]) -> CmdResult<Vec<PathBuf>> {
    projectors
        .iter()
        .map(|(_, path)| match from {
            Source::Folder(folder) => Ok(dir.join(folder).join(file_name(path)?)),
            Source::Files(files) => files
                .get(path)
                .map(|f| dir.join(f))
                .ok_or_else(|| format!("`from` names no file for {path}").into()),
        })
        .collect()
}

/// Warnings for files that are not one run of one camera.
fn mixed_runs(files: &[MeasurementFile], paths: &[(NodeId, String)]) -> Vec<String> {
    let mut out = Vec::new();
    let runs: Vec<&str> = files.iter().map(|f| f.run.as_str()).collect();
    if runs.windows(2).any(|w| matches!(w, [a, b] if a != b)) || runs.iter().any(|r| r.is_empty()) {
        out.push(
            "the measurements come from different runs: their readings compare only if the camera, its \
             exposure and white balance and the patch of wall were the same"
                .to_string(),
        );
    }
    let cams: Vec<(&str, &str)> =
        files.iter().map(|f| (f.camera.device.as_str(), f.camera.mode.as_str())).collect();
    if cams.windows(2).any(|w| matches!(w, [a, b] if a != b)) {
        out.push("the measurements were taken with different cameras or camera modes".to_string());
    }
    for (f, (_, path)) in files.iter().zip(paths) {
        if !f.shared_spot && files.len() > 1 {
            out.push(format!(
                "{path}: measured on its own part of the wall, not where all projectors of its run overlap"
            ));
        }
    }
    out
}

/// `projector.match` with `from`: the files read, matched, the params written.
pub fn run(e: &Engine, a: &MatchArgs, from: &Source) -> CmdResult<Json> {
    if a.camera.is_some() {
        return Err("`from` matches without the camera: give `camera` or `from`, not both".into());
    }
    let camera_only = [
        ("levels", a.levels.is_some()),
        ("settle", a.settle.is_some()),
        ("area", a.area.is_some()),
        ("timeout", a.timeout.is_some()),
        ("refine", a.refine.is_some()),
        ("grey_levels", a.grey_levels.is_some()),
        ("save", a.save.is_some()),
        ("note", a.note.is_some()),
        ("target", a.target != colour::Target::Reference),
    ];
    if let Some((name, _)) = camera_only.iter().find(|(_, given)| *given) {
        return Err(format!(
            "`{name}` needs the camera; with `from` only `projectors`, `samples` and `apply`"
        )
        .into());
    }
    let samples = a.samples.unwrap_or(crate::projector::DEFAULT_SAMPLES);
    if !(2..=penumbra_gpu::projection::MAX_CURVE).contains(&samples) {
        return Err(format!("`samples` is 2 to {}", penumbra_gpu::projection::MAX_CURVE).into());
    }
    let session = e.session();
    let dir = session.dir().to_path_buf();
    let net = session.doc().snapshot();
    let listed: Option<Vec<NodePath>> = match (&a.projectors, from) {
        (Some(p), _) => Some(p.clone()),
        (None, Source::Files(files)) => {
            Some(files.keys().map(|k| NodePath::parse(k)).collect::<Result<_, _>>()?)
        }
        (None, Source::Folder(_)) => None,
    };
    let projectors = projector_list(e, &net, listed.as_deref())?;
    if projectors.is_empty() {
        return Err("no projector to match".into());
    }
    let paths = files_of(&dir, from, &projectors)?;
    let files: Vec<MeasurementFile> = paths.iter().map(|p| read(p)).collect::<CmdResult<_>>()?;
    let stored_list: Vec<Stored> = files.iter().map(MeasurementFile::stored).collect();
    let named = |note: &colour::Note| note.named(|k| projectors.get(k).map(|(_, p)| p.clone()));
    let matched = stored::match_stored(&stored_list, samples).map_err(|n| named(&n))?;
    let mut warnings = mixed_runs(&files, &projectors);
    warnings.extend(matched.warnings.iter().map(named));

    let mut ops = Vec::new();
    let mut report = Vec::new();
    for (((_, path), c), (file, at)) in
        projectors.iter().zip(&matched.projectors).zip(files.iter().zip(&paths))
    {
        let matrix = correct::matrix_text(&c.matrix);
        let curves = correct::curves_text(&c.curves);
        let node = NodePath::parse(path)?;
        for (name, value) in [
            ("matrix", Value::Text(matrix.clone())),
            ("curves", Value::Text(curves.clone())),
            ("correct", Value::Bool(true)),
        ] {
            ops.push(Op::Set { param: ParamPath::new(node.clone(), Name::new(name)?)?, value });
        }
        report.push(json!({
            "path": path,
            "file": at.display().to_string(),
            "measured_through": file.projector,
            "run": file.run,
            "unix_time": file.unix_time,
            "note": file.note,
            "greys": file.greys.is_some(),
            "white": c.white,
            "white_signal": c.white_signal,
            "peak": {"strength_1": c.peak[0], "strength_0_5": c.peak[1]},
            "matrix": matrix,
            "curves": curves,
        }));
    }
    let apply = a.apply.unwrap_or(true);
    let history = if apply { Some(outcome(&edit_in(&session, &ops, "projector.match")?)) } else { None };
    let limiter = |k: usize| projectors.get(k).map(|(_, p)| p.clone()).unwrap_or_default();
    let limited: Vec<String> = matched.limited_by.iter().map(|k| limiter(*k)).collect();
    let peaks: Vec<String> =
        matched.projectors.iter().map(|c| format!("{:.0} %", c.peak[0] * 100.0)).collect();
    Ok(json!({
        "message": format!(
            "{} projector(s) matched from stored measurements, keeping about {} of their white{}",
            projectors.len(),
            peaks.join(" / "),
            if apply { "" } else { " (not applied)" }
        ),
        "from": "stored",
        "target_white": matched.target_white,
        "limited_by": {"red": limited.first(), "green": limited.get(1), "blue": limited.get(2)},
        "projectors": report,
        "applied": apply,
        "history": history,
        "warnings": warnings,
    }))
}

/// Greys the stored match is compared on against a camera run's result.
const CHECK_GREYS: [f64; 4] = [0.25, 0.5, 0.75, 1.0];

/// The signal grey `x` gets per channel through `matrix` and `curves`.
fn grey_signal(matrix: &colour::Mat3, curves: &[Vec<f64>; 3], x: f64) -> [f64; 3] {
    colour::signal(matrix, curves, [x; 3])
}

/// What matching from the run's own measurements (as if they had been stored) gives against
/// what the camera run settled on: per projector the largest difference of the signal, per
/// channel, over a few greys. Small differences say the files are good enough to match from.
pub(crate) fn check_against(
    stored_list: &[Stored],
    paths: &[(NodeId, String)],
    matrices: &[colour::Mat3],
    curves: &[[Vec<f64>; 3]],
    samples: usize,
) -> Json {
    let matched = match stored::match_stored(stored_list, samples) {
        Ok(m) => m,
        Err(note) => return json!({"error": note.text}),
    };
    let mut out = Vec::new();
    for (((_, path), s), (m, c)) in paths.iter().zip(&matched.projectors).zip(matrices.iter().zip(curves)) {
        let mut worst = [0.0_f64; 3];
        for x in CHECK_GREYS {
            let a = grey_signal(&s.matrix, &s.curves, x);
            let b = grey_signal(m, c, x);
            for ((w, a), b) in worst.iter_mut().zip(a).zip(b) {
                *w = w.max((a - b).abs());
            }
        }
        out.push(json!({"path": path, "largest_signal_difference": worst}));
    }
    json!(out)
}
