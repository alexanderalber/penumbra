//! `projector.match`: measures the projectors with a camera and writes their colour correction
//! (`out.projector`'s *colour* params), so projectors of different models look alike.
//!
//! Like `scan.decode`, the command runs on the caller's thread (the control channel's
//! connection, or a thread the UI starts) and only waits there: the cook thread keeps cooking
//! and the outputs keep presenting. It changes what the projectors show through the output
//! board's flat signals ([`penumbra_ops::out::BoardState::flat`]), never through the network,
//! and clears them when it ends, however it ends.
//!
//! Each picture is taken only after the change reached the projectors: the cook thread
//! published an output frame with it, every output window presented that frame, `settle`
//! passed, and then the camera delivered a frame that arrived after all that.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use penumbra_calib::colour::{self, Measured, Picture, Planes};
use penumbra_camera::{Key, Matrix};
use penumbra_core::{Name, NodeId, NodePath, ParamPath, Value};
use penumbra_graph::{Network, Op};
use penumbra_ops::out::{OutputBoard, PROJECTOR, correct};
use penumbra_ops::tex::video_in::VIDEO_IN;
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, FnCommand, Registry, args};
use crate::commands::{edit, outcome, require};
use crate::engine::Engine;

type CmdResult<T> = std::result::Result<T, CommandError>;

pub const DEFAULT_LEVELS: u32 = 8;
pub const DEFAULT_SETTLE: f64 = 0.5;
pub const DEFAULT_SAMPLES: usize = 17;
pub const DEFAULT_TIMEOUT: f64 = 5.0;
/// Widest camera picture measured; larger frames are sampled every few pixels.
const MAX_WIDTH: usize = 640;

/// One measurement at a time: two would fight over the projectors.
static RUNNING: AtomicBool = AtomicBool::new(false);

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let e = Arc::clone(engine);
    registry.register(FnCommand::new(
        "projector.match",
        "Measure the projectors with a camera (each alone: black, white, red, green and blue ramps) and \
         write their colour correction (matrix, curves) so they look alike; blocks for several seconds per \
         projector; {camera (tex.video_in path), projectors? (paths, default every active out.projector), \
         levels? (8), settle? (s, 0.5), samples? (curve samples, 17), area? ([x, y, w, h] of the camera \
         picture, 0..1), timeout? (s, 5), apply? (true: write the params as one undo step)}",
        move |p| {
            let a: MatchArgs = args(p)?;
            run(&e, &a)
        },
    ));
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatchArgs {
    pub camera: NodePath,
    pub projectors: Option<Vec<NodePath>>,
    pub levels: Option<u32>,
    pub settle: Option<f64>,
    pub samples: Option<usize>,
    pub area: Option<[f64; 4]>,
    pub timeout: Option<f64>,
    pub apply: Option<bool>,
}

/// Clears the flat signals of the projectors measured and the running flag when dropped.
struct Restore<'a> {
    board: &'a OutputBoard,
    nodes: Vec<NodeId>,
}

impl Drop for Restore<'_> {
    fn drop(&mut self) {
        self.board.update(|b| {
            for n in &self.nodes {
                b.flat.remove(n);
            }
        });
        RUNNING.store(false, Ordering::Release);
    }
}

/// What a measurement needs while it runs.
struct Rig<'a> {
    engine: &'a Engine,
    projectors: Vec<(NodeId, String)>,
    camera: String,
    key: Key,
    range: String,
    settle: Duration,
    timeout: Duration,
    /// The output pass last waited for.
    pass: u64,
}

impl Rig<'_> {
    /// Shows `signal(k)` on projector `k` (sRGB-encoded, uncorrected) and returns the camera's
    /// picture of it.
    fn shot(&mut self, signal: impl Fn(usize) -> [f32; 3]) -> CmdResult<Picture> {
        let wanted: BTreeMap<NodeId, [f32; 3]> =
            self.projectors.iter().enumerate().map(|(k, (id, _))| (*id, signal(k))).collect();
        let board = self.engine.board();
        board.update(|b| b.flat.extend(wanted.iter().map(|(k, v)| (*k, *v))));
        let deadline = Instant::now() + self.timeout;

        // The cook thread published a frame with it.
        let pass = loop {
            let frame = self.engine.wait_output(self.pass, Duration::from_millis(50));
            self.pass = self.pass.max(frame.pass);
            let mut done = true;
            for (id, path) in &self.projectors {
                match frame.projectors.iter().find(|p| p.node == *id) {
                    None => return Err(format!("{path} is gone").into()),
                    Some(p) if !p.active => return Err(format!("{path} was switched off").into()),
                    Some(p) => done &= p.flat == wanted.get(id).copied(),
                }
            }
            if done {
                break frame.pass;
            }
            if Instant::now() >= deadline {
                return Err("the cook thread published no frame in time".into());
            }
        };

        // Every window presented it.
        loop {
            let state = board.state();
            if !state.managed {
                break;
            }
            let late: Vec<&str> = self
                .projectors
                .iter()
                .filter(|(id, _)| state.presented.get(id).is_none_or(|p| *p < pass))
                .map(|(_, path)| path.as_str())
                .collect();
            if late.is_empty() {
                break;
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "{} did not show the measurement (no output window presented it; see output.list)",
                    late.join(", ")
                )
                .into());
            }
            std::thread::sleep(Duration::from_millis(2));
        }

        std::thread::sleep(self.settle);
        let after = Instant::now();
        let deadline = after + self.timeout;
        let hub = self.engine.camera();
        loop {
            if let Some(f) = hub.latest(&self.key).filter(|f| f.arrived >= after) {
                let full = match self.range.as_str() {
                    "limited" => false,
                    "full" => true,
                    _ => f.full_range,
                };
                let planes = Planes {
                    width: f.width,
                    height: f.height,
                    y: &f.y,
                    cb: &f.cb,
                    cr: &f.cr,
                    shift: f.shift,
                    kr_kb: match f.matrix {
                        Matrix::Bt601 => (0.299, 0.114),
                        Matrix::Bt709 => (0.2126, 0.0722),
                    },
                    full_range: full,
                };
                let step = (f.width as usize).div_ceil(MAX_WIDTH).max(1);
                return colour::linear_picture(&planes, step)
                    .map_err(|e| format!("{}: {e}", self.camera).into());
            }
            if let Some(st) = hub.stream_status(&self.key)
                && let (false, Some(e)) = (st.open, st.error)
            {
                return Err(format!("camera {}: {e}", self.camera).into());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "camera {} sent no frame for {:.1} s (stalled? video_in.status)",
                    self.camera,
                    self.timeout.as_secs_f64()
                )
                .into());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

fn text_param(net: &Network, e: &Engine, id: NodeId, kind: &str, name: &str) -> String {
    let (Some(node), Some(desc)) = (net.node(id), e.types().desc(kind)) else { return String::new() };
    node.param(desc, name).and_then(Value::as_str).map(str::to_string).unwrap_or_default()
}

pub fn run(e: &Engine, a: &MatchArgs) -> CmdResult<Json> {
    let started = Instant::now();
    let levels = a.levels.unwrap_or(DEFAULT_LEVELS);
    if !(2..=32).contains(&levels) {
        return Err("`levels` is 2 to 32".into());
    }
    let settle = a.settle.unwrap_or(DEFAULT_SETTLE);
    if !(0.0..=10.0).contains(&settle) {
        return Err("`settle` is 0 to 10 seconds".into());
    }
    let timeout = a.timeout.unwrap_or(DEFAULT_TIMEOUT);
    if !(0.05..=60.0).contains(&timeout) {
        return Err("`timeout` is 0.05 to 60 seconds".into());
    }
    let samples = a.samples.unwrap_or(DEFAULT_SAMPLES);
    if !(2..=penumbra_gpu::projection::MAX_CURVE).contains(&samples) {
        return Err(format!("`samples` is 2 to {}", penumbra_gpu::projection::MAX_CURVE).into());
    }
    if let Some(r) = a.area
        && !(r.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) && r[2] > 0.0 && r[3] > 0.0)
    {
        return Err("`area` is [x, y, w, h] within 0..1 of the camera picture".into());
    }

    let net = e.session().doc().snapshot();
    let projectors: Vec<(NodeId, String)> = match &a.projectors {
        Some(paths) => {
            let mut out = Vec::new();
            for path in paths {
                let id = require(&net, path)?;
                if net.node(id).is_none_or(|n| n.kind != PROJECTOR) {
                    return Err(format!("{path} is not an {PROJECTOR}").into());
                }
                if out.iter().any(|(o, _)| *o == id) {
                    return Err(format!("{path} is listed twice").into());
                }
                out.push((id, path.to_string()));
            }
            out
        }
        None => {
            let desc = e.types().desc(PROJECTOR);
            net.nodes()
                .filter(|n| n.kind == PROJECTOR)
                .filter(|n| !matches!(desc.and_then(|d| n.param(d, "active")), Some(Value::Bool(false))))
                .map(|n| (n.id, net.path_of(n.id).map(|p| p.to_string()).unwrap_or_default()))
                .collect()
        }
    };
    if projectors.is_empty() {
        return Err(format!("no active {PROJECTOR} to measure").into());
    }
    let cam = require(&net, &a.camera)?;
    if net.node(cam).is_none_or(|n| n.kind != VIDEO_IN) {
        return Err(format!("{} is not a {VIDEO_IN}", a.camera).into());
    }
    let key =
        Key::new(&text_param(&net, e, cam, VIDEO_IN, "device"), &text_param(&net, e, cam, VIDEO_IN, "mode"));
    match e.camera().stream_status(&key) {
        None => {
            return Err(
                format!("camera {} is not running (video_in.status; is camera input on?)", a.camera).into()
            );
        }
        Some(st) if !st.open => {
            let why = st.error.unwrap_or_else(|| "not open yet".into());
            return Err(format!("camera {}: {why}", a.camera).into());
        }
        Some(_) => {}
    }

    if RUNNING.swap(true, Ordering::AcqRel) {
        return Err("a projector.match is already running".into());
    }
    let restore = Restore { board: e.board(), nodes: projectors.iter().map(|(id, _)| *id).collect() };
    let mut rig = Rig {
        engine: e,
        projectors: projectors.clone(),
        camera: a.camera.to_string(),
        key,
        range: text_param(&net, e, cam, VIDEO_IN, "range"),
        settle: Duration::from_secs_f64(settle),
        timeout: Duration::from_secs_f64(timeout),
        pass: e.output().pass,
    };

    // Where each projector lights the camera picture.
    let black = rig.shot(|_| [0.0; 3])?;
    let mut whites = Vec::with_capacity(projectors.len());
    let mut masks = Vec::with_capacity(projectors.len());
    for (k, (_, path)) in projectors.iter().enumerate() {
        let white = rig.shot(|j| if j == k { [1.0; 3] } else { [0.0; 3] })?;
        let mut mask = colour::lit(&white, &black).ok_or_else(|| {
            format!("{path} lights no camera pixel (is it in the camera's view, on, and the camera's exposure right?)")
        })?;
        if let Some([x, y, w, h]) = a.area {
            let (pw, ph) = (black.width as f64, black.height as f64);
            for (i, m) in mask.iter_mut().enumerate() {
                let (px, py) = ((i % black.width) as f64 + 0.5, (i / black.width) as f64 + 0.5);
                *m &= px >= x * pw && px < (x + w) * pw && py >= y * ph && py < (y + h) * ph;
            }
            if colour::count(&mask) < colour::MIN_PIXELS {
                return Err(format!("{path} lights (almost) nothing inside `area`").into());
            }
        }
        whites.push(white);
        masks.push(mask);
    }
    let spots = colour::spots(&masks, black.width, black.height);
    let mut warnings = Vec::new();
    if projectors.len() > 1 && !spots.shared {
        warnings.push(
            "the projectors do not overlap in the camera's view: each was measured on its own part of the wall, so \
             differences of the wall and the camera's vignetting count as projector differences"
                .to_string(),
        );
    }

    // The ramps.
    let level_values: Vec<f64> = (1..=levels).map(|i| f64::from(i) / f64::from(levels)).collect();
    let mut measured = Vec::with_capacity(projectors.len());
    let mut white_means = Vec::with_capacity(projectors.len());
    let mut blacks = Vec::with_capacity(projectors.len());
    for (k, (_, path)) in projectors.iter().enumerate() {
        let spot = spots.masks.get(k).map(Vec::as_slice).unwrap_or(&[]);
        let mean = |p: &Picture| {
            colour::trimmed_mean(p, spot).ok_or_else(|| format!("{path}: an empty measuring spot"))
        };
        let b = mean(&black)?;
        let sub = |v: [f64; 3]| [v[0] - b[0], v[1] - b[1], v[2] - b[2]];
        let w = whites.get(k).map(mean).transpose()?.ok_or("lost a picture")?;
        if w.iter().any(|v| *v > 0.97) {
            warnings.push(format!("{path}: the camera clips its white; lower the camera's exposure"));
        }
        let mut ramps: [Vec<[f64; 3]>; 3] = Default::default();
        for (c, ramp) in ramps.iter_mut().enumerate() {
            for l in &level_values {
                let mut s = [0.0; 3];
                if let Some(v) = s.get_mut(c) {
                    *v = *l as f32;
                }
                let pic = rig.shot(|j| if j == k { s } else { [0.0; 3] })?;
                ramp.push(sub(mean(&pic)?));
            }
        }
        blacks.push(b);
        white_means.push(Some(sub(w)));
        measured.push(Measured { levels: level_values.clone(), ramps });
    }
    drop(restore);

    let matched = colour::match_projectors(&measured, &white_means, samples).map_err(|err| {
        // Name the projector instead of its index.
        let mut err = err;
        for (k, (_, path)) in projectors.iter().enumerate() {
            err = err.replace(&format!("projector {k}:"), &format!("{path}:"));
        }
        err
    })?;
    warnings.extend(matched.warnings.iter().map(|w| {
        let mut w = w.clone();
        for (k, (_, path)) in projectors.iter().enumerate() {
            w = w.replace(&format!("projector {k}:"), &format!("{path}:"));
        }
        w
    }));

    let mut ops = Vec::new();
    let mut report = Vec::new();
    for (k, ((_, path), c)) in projectors.iter().zip(&matched.projectors).enumerate() {
        let matrix = correct::matrix_text(&c.matrix);
        let curves = correct::curves_text(&c.curves);
        let node = NodePath::parse(path)?;
        for (name, value) in [
            ("matrix", Value::Text(matrix.clone())),
            ("curves", Value::Text(curves.clone())),
            ("correct", Value::Bool(true)),
        ] {
            let param = ParamPath::new(node.clone(), Name::new(name)?)?;
            ops.push(Op::Set { param, value });
        }
        let m = measured.get(k);
        report.push(json!({
            "path": path,
            "black": blacks.get(k),
            "white": white_means.get(k),
            "primaries": c.primaries,
            "levels": m.map(|m| &m.levels),
            "ramps": m.map(|m| &m.ramps),
            "response": c.response,
            "additivity": c.additivity,
            "peak": {"strength_1": c.peak[0], "strength_0_5": c.peak[1]},
            "matrix": matrix,
            "curves": curves,
        }));
    }
    let apply = a.apply.unwrap_or(true);
    let history = if apply { Some(outcome(&edit(e, &ops, "projector.match")?)) } else { None };
    let reference = projectors.get(matched.reference).map(|(_, p)| p.clone());
    Ok(json!({
        "message": format!(
            "{} projector(s) matched to {} at {:.0} % of its brightness{}",
            projectors.len(),
            reference.as_deref().unwrap_or("?"),
            matched.alpha * 100.0,
            if apply { "" } else { " (not applied)" }
        ),
        "reference": reference,
        "alpha": matched.alpha,
        "shared_spot": spots.shared,
        "spot_pixels": spots.masks.iter().map(|m| colour::count(m)).collect::<Vec<_>>(),
        "camera_picture": [black.width, black.height],
        "projectors": report,
        "applied": apply,
        "history": history,
        "warnings": warnings,
        "seconds": started.elapsed().as_secs_f64(),
    }))
}
