//! `tex.scan`: one projector's structured light scan, held. The node names the projector, the
//! camera and the folder; its scan button (`scan.run`) has the engine take the scan off the cook
//! thread, and the node reads the files the scan wrote: the projector-to-camera map, the blend
//! weight and the scene as the projector sees it. Like a locked node it keeps what it has until
//! a new scan replaces it; the files are its saved state, so an opened project needs no rescan.
//!
//! Nothing runs per frame: the files are decoded off the cook thread once, and again only when
//! they change on disk (not while a scan is writing them, but once it ended).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use half::f16;
use penumbra_core::{Error, NodeId, ParamSpec, lock};
use penumbra_gpu::Tex;
use penumbra_gpu::image::Decoded;
use penumbra_graph::{
    AsyncSlot, CookContext, CookError, Cooked, Data, DataKind, FileWatch, FnNodeType, JobStatus, NodeDesc,
    NodeTypes, Operator, Waker,
};
use serde::Serialize;

pub const SCAN: &str = "tex.scan";

/// Files of a projector's scan folder the node reads (`scan.capture` and `scan.blend` write them).
pub const PROJECTOR_MAP: &str = "proj_to_cam.exr";
pub const SMOOTH_MAP: &str = "proj_to_cam_smooth.exr";
pub const WEIGHT: &str = "weight.png";
pub const SCENE: &str = "scene.png";

/// Where scans go unless the node says otherwise (relative to the project).
pub const DEFAULT_FOLDER: &str = "scan";

/// The folder name of a projector's scan: its path without the leading slash, slashes as `_`.
pub fn folder_name(path: &str) -> String {
    let name = path.trim_start_matches('/').replace(['/', '\\', ':'], "_");
    if name.is_empty() { "projector".into() } else { name }
}

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            SCAN,
            "One projector's structured light scan, held. The scan button (scan.run) measures the projector \
             with the camera; the outputs are the maps the scan wrote, kept until the next scan",
        )
        .param(ParamSpec::text("projector", "").doc("The out.projector scanned (a path, or a name beside this node)"))
        .param(ParamSpec::text("camera", "").doc(
            "The tex.video_in that films the projectors; the scan button scans every tex.scan naming the same camera",
        ))
        .param(ParamSpec::text("folder", DEFAULT_FOLDER).doc(
            "Where scans go, relative to the project. Inside, the projector's scan is the folder named after the projector's path",
        ))
        .param(ParamSpec::text("mapper", "").doc(
            "The tex.mapper feeding the projector. When every tex.scan of a run names its mapper, the run lines the mappers up (scan.align)",
        ))
        .param(ParamSpec::bool("smooth", true).doc(
            "proj_to_cam is the map smoothed within the decoded area, for clean remapped edges. The run's \
             blend writes the smoothed map. Off: the map as decoded",
        ))
        .param(
            ParamSpec::float("settle", 0.05)
                .clamp(0.0, 10.0)
                .range(0.0, 0.5)
                .more()
                .doc("Seconds waited past the measured light-to-camera latency before a picture is taken"),
        )
        .param(
            ParamSpec::float("level", 1.0)
                .clamp(0.05, 1.0)
                .more()
                .doc("Brightness of the lit stripes; lower it only if the camera clips even with its exposure turned down"),
        )
        .param(
            ParamSpec::int("fill", 32)
                .clamp(0.0, 1024.0)
                .range(0.0, 128.0)
                .more()
                .doc("Projector pixels the decode fills in from measured neighbours"),
        )
        .output("proj_to_cam", DataKind::Tex)
        .describe_output(
            "proj_to_cam",
            "Projector size: per projector pixel, the camera position in red and green (0 to 1). Blue: 1 \
             measured, 0.5 filled in, -1 none; -1 everywhere before a scan. Use as tex.remap's map with the \
             camera's picture as in",
        )
        .output("weight", DataKind::Tex)
        .describe_output(
            "weight",
            "Projector size: the blend weight against the other projectors of the run; multiply the \
             projector's picture by it. 1 everywhere for a projector alone, or before a blend",
        )
        .output("scene", DataKind::Tex)
        .describe_output("scene", "Projector size: the scene as the projector sees it (black before a scan)"),
        || Box::new(Scan::default()) as Box<dyn Operator>,
    ))
}

/// How a node's scan stands, as the UI and `scan.status` show it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ScanState {
    /// A run that includes this node is going on.
    pub running: bool,
    /// 0 to 1 while running.
    pub progress: Option<f32>,
    /// What the run is doing now, or how the last one ended.
    pub step: String,
    /// The last run failed (or was cancelled) with this.
    pub error: Option<String>,
    pub warnings: Vec<String>,
    /// Runs that ended for this node; a change reloads the files.
    pub finished: u64,
}

/// The scans the engine runs, by node. Provided by the engine as a cooker resource; cheap to
/// clone. At most one run at a time.
#[derive(Clone, Default)]
pub struct Scans(Arc<Mutex<Inner>>);

#[derive(Default)]
struct Inner {
    nodes: BTreeMap<NodeId, (ScanState, Option<Waker>)>,
    /// The cancel flag of the run going on.
    run: Option<Arc<AtomicBool>>,
    /// Counts [`Scans::clear`]: a run started before a clear no longer reports.
    epoch: u64,
}

/// A run's claim on [`Scans`]: its cancel flag and the epoch it reports under.
#[derive(Clone)]
pub struct RunTicket {
    pub cancel: Arc<AtomicBool>,
    epoch: u64,
}

impl RunTicket {
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

impl Scans {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts a run for `nodes`; fails while another runs.
    pub fn begin(&self, nodes: &[NodeId]) -> Result<RunTicket, String> {
        let mut inner = lock(&self.0);
        if inner.run.is_some() {
            return Err("a scan is already running (scan.cancel stops it)".into());
        }
        let cancel = Arc::new(AtomicBool::new(false));
        inner.run = Some(cancel.clone());
        let epoch = inner.epoch;
        for id in nodes {
            let (state, waker) = inner.nodes.entry(*id).or_default();
            state.running = true;
            state.progress = Some(0.0);
            state.step = "starting".into();
            state.error = None;
            state.warnings.clear();
            if let Some(w) = waker {
                w.wake();
            }
        }
        Ok(RunTicket { cancel, epoch })
    }

    /// Changes a node's state for the run of `ticket` (nothing once the run is stale) and wakes
    /// the node.
    pub fn update(&self, ticket: &RunTicket, node: NodeId, f: impl FnOnce(&mut ScanState)) {
        let mut inner = lock(&self.0);
        if inner.epoch != ticket.epoch {
            return;
        }
        let (state, waker) = inner.nodes.entry(node).or_default();
        f(state);
        if let Some(w) = waker {
            w.wake();
        }
    }

    /// Ends the run of `ticket`: every node it names stops running and counts a finished run.
    pub fn end(&self, ticket: &RunTicket, nodes: &[NodeId]) {
        let mut inner = lock(&self.0);
        if inner.run.as_ref().is_some_and(|r| Arc::ptr_eq(r, &ticket.cancel)) {
            inner.run = None;
        }
        if inner.epoch != ticket.epoch {
            return;
        }
        for id in nodes {
            let (state, waker) = inner.nodes.entry(*id).or_default();
            state.running = false;
            state.progress = None;
            state.finished += 1;
            if let Some(w) = waker {
                w.wake();
            }
        }
    }

    /// Asks the run going on to stop; false when none runs.
    pub fn cancel(&self) -> bool {
        match &lock(&self.0).run {
            Some(c) => {
                c.store(true, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    pub fn running(&self) -> bool {
        lock(&self.0).run.is_some()
    }

    pub fn state(&self, node: NodeId) -> Option<ScanState> {
        lock(&self.0).nodes.get(&node).map(|(s, _)| s.clone())
    }

    /// Forgets every node (another project opened: node ids are reused) and cancels the run,
    /// whose reports are dropped from now on.
    pub fn clear(&self) {
        let mut inner = lock(&self.0);
        if let Some(c) = &inner.run {
            c.store(true, Ordering::Relaxed);
        }
        inner.nodes.clear();
        inner.epoch += 1;
    }

    fn check_in(&self, node: NodeId, waker: Waker) -> ScanState {
        let mut inner = lock(&self.0);
        let (state, w) = inner.nodes.entry(node).or_default();
        *w = Some(waker);
        state.clone()
    }
}

/// The node's pictures: proj_to_cam, weight, scene.
type Loaded = [Tex; 3];

#[derive(Default)]
struct Scan {
    slot: AsyncSlot<Loaded>,
    /// The folder loaded, and its files watched.
    folder: Option<PathBuf>,
    smooth: bool,
    watches: Vec<FileWatch>,
    /// [`ScanState::finished`] when the files were last loaded.
    finished: u64,
    state: ScanState,
}

/// The folder a node's scan is in: `folder` (relative to `project`) and the projector's name.
pub fn scan_folder(project: &Path, folder: &str, projector: &str) -> PathBuf {
    let folder = match folder.trim() {
        "" => DEFAULT_FOLDER,
        f => f,
    };
    project.join(folder).join(folder_name(projector.trim()))
}

/// Whether `folder` holds a scan the node can show.
pub fn has_scan(folder: &Path) -> bool {
    folder.join(PROJECTOR_MAP).exists()
}

/// A 1 x 1 picture of `value` in every channel.
fn flat(value: f32) -> Decoded {
    let v = f16::from_f32(value);
    Decoded { width: 1, height: 1, rgba: vec![v, v, v, f16::ONE], source: "flat".into() }
}

/// Decodes the files of a scan folder: the map (smoothed if asked and written, else as decoded),
/// the weight (1 everywhere until a blend wrote one) and the scene.
fn load(folder: &Path, smooth: bool, max_side: u32) -> Result<[Decoded; 3], String> {
    let raw = folder.join(PROJECTOR_MAP);
    if !raw.exists() {
        // Nothing scanned yet (the param panel says so): no camera position anywhere.
        return Ok([flat(-1.0), flat(1.0), flat(0.0)]);
    }
    let smoothed = folder.join(SMOOTH_MAP);
    let map_file = if smooth && smoothed.exists() { smoothed } else { raw };
    let map = penumbra_gpu::image::decode_file(&map_file, max_side).map_err(|e| e.0)?;
    let weight_file = folder.join(WEIGHT);
    let weight = if weight_file.exists() {
        penumbra_gpu::image::decode_file(&weight_file, max_side).map_err(|e| e.0)?
    } else {
        flat(1.0)
    };
    let scene_file = folder.join(SCENE);
    let scene = if scene_file.exists() {
        penumbra_gpu::image::decode_file(&scene_file, max_side).map_err(|e| e.0)?
    } else {
        flat(0.0)
    };
    Ok([map, weight, scene])
}

impl Operator for Scan {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let fresh = self.slot.poll();
        let gpu = super::gpu(cx)?;
        self.state = cx.resource::<Scans>().map(|s| s.check_in(cx.node(), cx.waker())).unwrap_or_default();
        let root = match cx.text("folder")?.trim() {
            "" => DEFAULT_FOLDER,
            f => f,
        };
        let folder = cx.resolve_path(root)?.join(folder_name(cx.text("projector")?.trim()));
        let smooth = cx.bool("smooth")?;
        let moved = self.folder.as_ref() != Some(&folder) || self.smooth != smooth;
        if moved {
            self.watches = [PROJECTOR_MAP, SMOOTH_MAP, WEIGHT, SCENE]
                .iter()
                .map(|f| cx.watch_file(&folder.join(f)))
                .collect();
        }
        // While a scan writes the files they are left alone (the maps held stay); once it ended
        // they are read again, changed or not.
        let mut changed = false;
        if !self.state.running {
            // Every flag taken, so one change reloads once.
            for w in &self.watches {
                changed |= w.take_changed();
            }
            changed |= self.state.finished != self.finished;
        }
        if moved || changed {
            self.folder = Some(folder.clone());
            self.smooth = smooth;
            self.finished = self.state.finished;
            let gpu = gpu.clone();
            self.slot.start(cx.spawn(move |_job| {
                let pictures = load(&folder, smooth, gpu.max_texture_size())?;
                let names = ["proj_to_cam", "weight", "scene"];
                let mut out: Vec<Tex> = Vec::with_capacity(3);
                for (p, name) in pictures.iter().zip(names) {
                    out.push(gpu.upload(p, &format!("{} {name}", folder.display())).map_err(|e| e.0)?);
                }
                match <[Tex; 3]>::try_from(out) {
                    Ok(t) => Ok(t),
                    Err(_) => Err("lost a picture".into()),
                }
            }));
        }
        if let Some(e) = &self.state.error {
            cx.warn(format!("the last scan failed: {e}"));
        }
        if let Some(e) = self.slot.error() {
            return Err(CookError::failed(e));
        }
        match (fresh, self.slot.latest()) {
            (true, Some(t)) => {
                Ok(Cooked::Outputs(t.iter().map(|t| Data::new(DataKind::Tex, t.clone())).collect()))
            }
            (_, None) => Err(CookError::Pending),
            (false, Some(_)) => Ok(Cooked::Unchanged),
        }
    }

    fn job_status(&self) -> Option<JobStatus> {
        if self.state.running {
            return Some(JobStatus::Running { progress: self.state.progress });
        }
        Some(self.slot.status())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_names_follow_the_path() {
        assert_eq!(folder_name("/proj_a"), "proj_a");
        assert_eq!(folder_name("/rig/proj_b"), "rig_proj_b");
        assert_eq!(folder_name("/"), "projector");
    }

    #[test]
    fn one_run_at_a_time_and_a_clear_drops_its_reports() {
        let s = Scans::new();
        let (a, b) = (NodeId(1), NodeId(2));
        let t = s.begin(&[a, b]).unwrap();
        assert!(s.running());
        assert!(s.begin(&[a]).is_err());
        assert!(s.state(a).unwrap().running);
        s.update(&t, a, |st| st.progress = Some(0.5));
        assert_eq!(s.state(a).unwrap().progress, Some(0.5));
        assert!(s.cancel() && t.cancelled());
        s.end(&t, &[a, b]);
        assert!(!s.running());
        let st = s.state(b).unwrap();
        assert!(!st.running && st.finished == 1 && st.progress.is_none());

        // A clear (another project) cancels the run and ignores what it reports later.
        let t = s.begin(&[a]).unwrap();
        s.clear();
        assert!(t.cancelled());
        s.update(&t, a, |st| st.step = "late".into());
        s.end(&t, &[a]);
        assert!(s.state(a).is_none());
        assert!(!s.running());
        assert!(!s.cancel());
    }
}
