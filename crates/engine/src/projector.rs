//! `projector.match`: measures the projectors with a camera and writes their colour correction
//! (`out.projector`'s *colour* params), so projectors of different models look alike.
//!
//! Like `scan.decode`, the command runs on the caller's thread (the control channel's
//! connection, or a thread the UI starts) and only waits there: the cook thread keeps cooking
//! and the outputs keep presenting. It changes what the projectors show through the output
//! board's flat signals ([`penumbra_ops::out::BoardState::flat`]), never through the network,
//! and clears them when it ends, however it ends.
//!
//! The matrix comes from camera values taken as linear light, which an ordinary camera is not;
//! so it is then refined in a closed loop (`refine` rounds): every other projector shows white
//! through its correction, and each row of its matrix is scaled until the camera reads that
//! channel of white as it reads the reference's, by a secant search (Broyden's, the channels interact through the camera) in the log of
//! the scales.
//! That holds whatever monotone curve the camera applies; white rather than each primary,
//! because cameras do not add up. The primaries and a grey are read once more and reported.
//!
//! Each picture is taken only after the change reached the projectors: the cook thread
//! published an output frame with it, every output window presented that frame, `settle`
//! passed, and then the camera delivered a frame that arrived after all that. `scan.capture`
//! waits the same way ([`wait_shown`], [`CameraStream`]).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use penumbra_calib::colour::{self, Mat3, Measured, Picture, Planes, Target};
use penumbra_camera::{CameraFrame, Key, Matrix};
use penumbra_core::{Name, NodeId, NodePath, ParamPath, Value};
use penumbra_graph::{Network, Op};
use penumbra_ops::out::{PROJECTOR, ProjectorFrame, correct};
use penumbra_ops::tex::video_in::VIDEO_IN;
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, FnCommand, Registry, args};
use crate::commands::{edit_in, outcome, require};
use crate::engine::Engine;
use crate::projector_stored::{self as stored_files, CameraOf, RunInfo, Source};

type CmdResult<T> = std::result::Result<T, CommandError>;

pub const DEFAULT_LEVELS: u32 = 8;
pub const DEFAULT_SETTLE: f64 = 0.5;
pub const DEFAULT_SAMPLES: usize = 17;
pub const DEFAULT_TIMEOUT: f64 = 5.0;
pub const DEFAULT_REFINE: u32 = 5;

/// `settle` (seconds to wait after the picture changed) of a projector command, checked.
pub(crate) fn settle_of(settle: Option<f64>, default: f64) -> CmdResult<f64> {
    let settle = settle.unwrap_or(default);
    if !(settle.is_finite() && (0.0..=10.0).contains(&settle)) {
        return Err("`settle` is 0 to 10 seconds".into());
    }
    Ok(settle)
}

/// `timeout` (seconds to wait for the camera) of a projector command, checked.
pub(crate) fn timeout_of(timeout: Option<f64>) -> CmdResult<f64> {
    let timeout = timeout.unwrap_or(DEFAULT_TIMEOUT);
    if !(timeout.is_finite() && (0.05..=60.0).contains(&timeout)) {
        return Err("`timeout` is 0.05 to 60 seconds".into());
    }
    Ok(timeout)
}
pub const DEFAULT_GREY_LEVELS: u32 = 3;
/// A grey between the refined ones, read for the report only: how well the curves interpolate.
const PROBE: f64 = 0.375;
/// Refining stops early when every channel reads within this (log ratio, about 0.5 %).
const REFINED: f64 = 0.005;
/// Widest camera picture measured; larger frames are sampled every few pixels.
pub(crate) const MAX_WIDTH: usize = 640;

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let e = Arc::clone(engine);
    registry.register(FnCommand::new(
        "projector.match",
        "Measure the projectors with a camera (each alone: black, white, red, green and blue ramps) and \
         write their colour correction (matrix, curves) so they look alike. Blocks for about half a \
         minute per projector; {camera (tex.video_in path), projectors? (paths, default every active \
         out.projector), levels? (8), settle? (s, 0.5), samples? (curve samples, 17), area? ([x, y, w, h] \
         of the camera picture, 0..1), timeout? (s, 5), target? (reference: the others respond like the \
         reference projector, the reference keeps straight curves, works through any camera | srgb: \
         every projector follows the sRGB curve, only with a camera known to be linear), refine? (rounds \
         of closed-loop correction against the reference, the matrix on white, then the curves on greys; \
         5; 0 off), grey_levels? (greys the curves are refined on, evenly spaced; 3), save? (folder, \
         relative to the project: also measures a grey ramp and writes each projector's measurement as \
         NAME.json with NAME the node name, to match from later), note? (text saved with them: settings, lamp hours), \
         apply? (true: write the params as one undo step)}. Without the camera: {from (folder of \
         NAME.json, or {projector path: file}), projectors?, samples?, apply?} matches from stored \
         measurements; each camera channel follows the projector dimmest there, so the common white lies \
         between the white points when no projector is the dimmest in every channel",
        move |p| {
            let a: MatchArgs = args(p)?;
            match &a.from {
                Some(from) => stored_files::run(&e, &a, from),
                None => run(&e, &a),
            }
        },
    ));
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatchArgs {
    pub camera: Option<NodePath>,
    /// Stored measurements to match from instead of the camera.
    pub from: Option<Source>,
    /// A folder (relative to the project) to save each projector's measurement in.
    pub save: Option<String>,
    /// Free text saved with the measurements.
    pub note: Option<String>,
    pub projectors: Option<Vec<NodePath>>,
    pub levels: Option<u32>,
    pub settle: Option<f64>,
    pub samples: Option<usize>,
    pub area: Option<[f64; 4]>,
    pub timeout: Option<f64>,
    pub apply: Option<bool>,
    pub refine: Option<u32>,
    pub grey_levels: Option<u32>,
    #[serde(default)]
    pub target: Target,
}

/// The projectors a measurement holds: while it lives no other measurement runs on the engine,
/// and when dropped (however the measurement ends) their flat signals and stripes are cleared.
pub(crate) struct Claim<'a> {
    engine: &'a Engine,
    nodes: Vec<NodeId>,
}

impl<'a> Claim<'a> {
    pub(crate) fn new(e: &'a Engine, nodes: Vec<NodeId>) -> CmdResult<Self> {
        if !e.begin_measurement() {
            return Err("a measurement (projector.match or scan.capture) is already running".into());
        }
        Ok(Self { engine: e, nodes })
    }
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        self.engine.board().update(|b| {
            for n in &self.nodes {
                b.flat.remove(n);
                b.stripes.remove(n);
            }
        });
        self.engine.end_measurement();
    }
}

/// The camera of a measurement: a running `tex.video_in`'s stream.
pub(crate) struct CameraStream {
    pub key: Key,
    /// Its node path, for messages.
    pub name: String,
    /// The node's `range` param (`auto`, `limited`, `full`).
    range: String,
}

impl CameraStream {
    /// The stream of the `tex.video_in` at `path`; fails unless it is open.
    pub(crate) fn open(e: &Engine, net: &Network, path: &NodePath) -> CmdResult<Self> {
        let cam = require(net, path)?;
        if net.node(cam).is_none_or(|n| n.kind != VIDEO_IN) {
            return Err(format!("{path} is not a {VIDEO_IN}").into());
        }
        let key = Key::new(
            &text_param(net, e, cam, VIDEO_IN, "device"),
            &text_param(net, e, cam, VIDEO_IN, "mode"),
        );
        match e.camera().stream_status(&key) {
            None => {
                return Err(
                    format!("camera {path} is not running (video_in.status; is camera input on?)").into()
                );
            }
            Some(st) if !st.open => {
                let why = st.error.unwrap_or_else(|| "not open yet".into());
                return Err(format!("camera {path}: {why}").into());
            }
            Some(_) => {}
        }
        Ok(Self { key, name: path.to_string(), range: text_param(net, e, cam, VIDEO_IN, "range") })
    }

    /// The latest frame once `accept` takes it, waiting up to `timeout`.
    pub(crate) fn next(
        &self,
        e: &Engine,
        timeout: Duration,
        accept: impl Fn(&CameraFrame) -> bool,
    ) -> CmdResult<Arc<CameraFrame>> {
        let deadline = Instant::now() + timeout;
        let hub = e.camera();
        loop {
            if let Some(f) = hub.latest(&self.key).filter(|f| accept(f)) {
                return Ok(f);
            }
            if let Some(st) = hub.stream_status(&self.key)
                && let (false, Some(err)) = (st.open, st.error)
            {
                return Err(format!("camera {}: {err}", self.name).into());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "camera {} sent no frame for {:.1} s (stalled? video_in.status)",
                    self.name,
                    timeout.as_secs_f64()
                )
                .into());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// The planes of `f` as the node reads them (its `range` param over the frame's own).
    pub(crate) fn planes<'f>(&self, f: &'f CameraFrame) -> Planes<'f> {
        let full = match self.range.as_str() {
            "limited" => false,
            "full" => true,
            _ => f.full_range,
        };
        Planes {
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
        }
    }
}

/// Waits until the cook thread published an output frame in which `ready` holds for every one
/// of `projectors` (each still there and active) and every output window presented it; returns
/// when that was seen. `pass` is the newest pass waited for so far, and is advanced.
pub(crate) fn wait_shown(
    e: &Engine,
    projectors: &[(NodeId, String)],
    pass: &mut u64,
    timeout: Duration,
    ready: impl Fn(&ProjectorFrame) -> bool,
) -> CmdResult<Instant> {
    let deadline = Instant::now() + timeout;
    // The cook thread published a frame with it.
    let shown = loop {
        let frame = e.wait_output(*pass, Duration::from_millis(50));
        *pass = (*pass).max(frame.pass);
        let mut done = true;
        for (id, path) in projectors {
            match frame.projectors.iter().find(|p| p.node == *id) {
                None => return Err(format!("{path} is gone").into()),
                Some(p) if !p.active => return Err(format!("{path} was switched off").into()),
                Some(p) => done &= ready(p),
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
    let board = e.board();
    loop {
        let state = board.state();
        if !state.managed {
            return Ok(Instant::now());
        }
        let late: Vec<&str> = projectors
            .iter()
            .filter(|(id, _)| state.presented.get(id).is_none_or(|p| *p < shown))
            .map(|(_, path)| path.as_str())
            .collect();
        if late.is_empty() {
            return Ok(Instant::now());
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
}

/// What a measurement needs while it runs.
pub(crate) struct Rig<'a> {
    pub engine: &'a Engine,
    pub projectors: Vec<(NodeId, String)>,
    pub camera: CameraStream,
    pub settle: Duration,
    pub timeout: Duration,
    /// The output pass last waited for.
    pub pass: u64,
    /// Camera frames averaged per picture (at least 1).
    pub frames: usize,
}

impl Rig<'_> {
    /// Shows `signal(k)` on projector `k` (sRGB-encoded, uncorrected) and returns the camera's
    /// picture of it: the mean of `frames` consecutive frames.
    pub(crate) fn shot(&mut self, signal: impl Fn(usize) -> [f32; 3]) -> CmdResult<Picture> {
        let wanted: BTreeMap<NodeId, [f32; 3]> =
            self.projectors.iter().enumerate().map(|(k, (id, _))| (*id, signal(k))).collect();
        self.engine.board().update(|b| b.flat.extend(wanted.iter().map(|(k, v)| (*k, *v))));
        wait_shown(self.engine, &self.projectors, &mut self.pass, self.timeout, |p| {
            p.flat == wanted.get(&p.node).copied()
        })?;
        std::thread::sleep(self.settle);
        let after = Instant::now();
        let mut f = self.camera.next(self.engine, self.timeout, |f| f.arrived >= after)?;
        let step = (f.width as usize).div_ceil(MAX_WIDTH).max(1);
        let read = |f: &CameraFrame| -> CmdResult<Picture> {
            colour::linear_picture(&self.camera.planes(f), step)
                .map_err(|e| format!("{}: {e}", self.camera.name).into())
        };
        let mut sum = read(&f)?;
        let n = self.frames.max(1);
        for _ in 1..n {
            let last = f.arrived;
            f = self.camera.next(self.engine, self.timeout, |g| g.arrived > last)?;
            let more = read(&f)?;
            if (more.width, more.height) != (sum.width, sum.height) {
                return Err(format!("{}: the frame size changed while measuring", self.camera.name).into());
            }
            for (s, m) in sum.rgb.iter_mut().zip(&more.rgb) {
                for (a, b) in s.iter_mut().zip(m) {
                    *a += *b;
                }
            }
        }
        let scale = 1.0 / n as f32;
        for p in &mut sum.rgb {
            for v in p.iter_mut() {
                *v *= scale;
            }
        }
        Ok(sum)
    }
}

/// Takes camera pictures of what the projectors show: [`Rig`] with a camera, a synthetic one
/// in tests.
pub(crate) trait Shoot {
    /// Shows `signal(k)` on projector `k` (sRGB-encoded, uncorrected) and returns the camera's
    /// picture of it.
    fn shoot(&mut self, signal: &dyn Fn(usize) -> [f32; 3]) -> CmdResult<Picture>;

    /// Projector `k` alone showing `s`, every other one black.
    fn alone(&mut self, k: usize, s: [f32; 3]) -> CmdResult<Picture> {
        self.shoot(&|j| if j == k { s } else { [0.0; 3] })
    }
}

impl Shoot for Rig<'_> {
    fn shoot(&mut self, signal: &dyn Fn(usize) -> [f32; 3]) -> CmdResult<Picture> {
        self.shot(signal)
    }
}

const NAMES: [&str; 3] = ["red", "green", "blue"];

/// What refining shows: white first (adjusted against), then full red, green, blue and grey at
/// half the drive (reported only).
const WHITE: [f64; 3] = [1.0; 3];
const PRIMARIES: [[f64; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// `curves` bent at the grey levels: channel c by `e^y[j][c]` at encoded drive `knots[j][c]`,
/// back to unchanged at `ends[c]` (white, matched by the matrix) and at black.
fn bend(curves: &[Vec<f64>; 3], knots: &[[f64; 3]], ends: [f64; 3], y: &[[f64; 3]]) -> [Vec<f64>; 3] {
    let mut out: [Vec<f64>; 3] = Default::default();
    for (c, (o, curve)) in out.iter_mut().zip(curves).enumerate() {
        // Unchanged from the table sample at or below white on: white falls between two samples,
        // and both must stay as they were for white to.
        let n = curve.len().saturating_sub(1).max(1) as f64;
        let end = (ends.get(c).copied().unwrap_or(1.0) * n).floor() / n;
        let mut points: Vec<(f64, f64)> = knots
            .iter()
            .zip(y)
            .filter_map(|(k, y)| Some((*k.get(c)?, *y.get(c)?)))
            .filter(|(k, _)| *k < end)
            .collect();
        points.push((end, 0.0));
        *o = colour::reshape(curve, &points);
    }
    out
}

/// Per channel the ratio of `got` to `want` (`None` where either is too dark to read) and the
/// difference in 8-bit camera codes.
fn ratios(got: [f64; 3], want: [f64; 3], black_got: [f64; 3], black_want: [f64; 3]) -> Json {
    let mut ratio = [None; 3];
    let mut codes = [0.0; 3];
    let code = |v: f64| colour::encode(v) * 255.0;
    let pairs = got.iter().zip(want).zip(black_got.into_iter().zip(black_want));
    for ((rc, cc), ((g, w), (bg, bw))) in ratio.iter_mut().zip(codes.iter_mut()).zip(pairs) {
        *rc = (*g > 1e-4 && w > 1e-4).then(|| g / w);
        *cc = code(bg + g) - code(bw + w);
    }
    json!({"ratio": ratio, "residual_codes": codes})
}

/// Where the camera reads one projector: its index, its measuring spot and its black there.
#[derive(Clone, Copy)]
struct Spot<'s> {
    k: usize,
    mask: &'s [bool],
    black: [f64; 3],
}

/// What the camera reads (black subtracted, in the spot) of the spot's projector alone showing
/// each of `contents` (linear light) through `matrix` and `curves`.
fn read_through(
    shoot: &mut impl Shoot,
    spot: Spot<'_>,
    matrix: &Mat3,
    curves: &[Vec<f64>; 3],
    contents: &[[f64; 3]],
) -> CmdResult<Vec<[f64; 3]>> {
    let mut out = Vec::with_capacity(contents.len());
    for content in contents {
        let s = colour::signal(matrix, curves, *content).map(|v| v as f32);
        let pic = shoot.alone(spot.k, s)?;
        let m = colour::trimmed_mean(&pic, spot.mask).ok_or("an empty measuring spot")?;
        out.push([m[0] - spot.black[0], m[1] - spot.black[1], m[2] - spot.black[2]]);
    }
    Ok(out)
}

/// Per channel the camera's reading `got` over `want`, as a log ratio and as the difference in
/// 8-bit camera codes (with each one's black added back); `None` where either is not light.
fn compare(
    got: [f64; 3],
    want: [f64; 3],
    black_got: [f64; 3],
    black_want: [f64; 3],
) -> Option<([f64; 3], [f64; 3])> {
    let code = |v: f64| colour::encode(v) * 255.0;
    let mut f = [0.0; 3];
    let mut codes = [0.0; 3];
    for ((fc, cc), ((g, w), (bg, bw))) in f
        .iter_mut()
        .zip(codes.iter_mut())
        .zip(got.into_iter().zip(want).zip(black_got.into_iter().zip(black_want)))
    {
        if !(g > 1e-6 && w > 1e-6) {
            return None;
        }
        *fc = (g / w).ln();
        *cc = code(bg + g) - code(bw + w);
    }
    Some((f, codes))
}

/// The `out.projector`s at `paths` (each once), or every active one; with their paths.
pub(crate) fn projector_list(
    e: &Engine,
    net: &Network,
    paths: Option<&[NodePath]>,
) -> CmdResult<Vec<(NodeId, String)>> {
    Ok(match paths {
        Some(paths) => {
            let mut out = Vec::new();
            for path in paths {
                let id = require(net, path)?;
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
    })
}

pub(crate) fn text_param(net: &Network, e: &Engine, id: NodeId, kind: &str, name: &str) -> String {
    let (Some(node), Some(desc)) = (net.node(id), e.types().desc(kind)) else { return String::new() };
    node.param(desc, name).and_then(Value::as_str).map(str::to_string).unwrap_or_default()
}

/// `projector.match`'s arguments, checked, the defaults filled in.
#[derive(Clone, Debug, PartialEq)]
struct Settings {
    levels: u32,
    settle: f64,
    timeout: f64,
    samples: usize,
    refine: u32,
    /// The greys the curves are refined on, evenly spaced between black and white.
    greys: Vec<f64>,
}

fn settings(a: &MatchArgs) -> CmdResult<Settings> {
    let levels = a.levels.unwrap_or(DEFAULT_LEVELS);
    if !(2..=32).contains(&levels) {
        return Err("`levels` is 2 to 32".into());
    }
    let settle = settle_of(a.settle, DEFAULT_SETTLE)?;
    let timeout = timeout_of(a.timeout)?;
    let samples = a.samples.unwrap_or(DEFAULT_SAMPLES);
    if !(2..=penumbra_gpu::projection::MAX_CURVE).contains(&samples) {
        return Err(format!("`samples` is 2 to {}", penumbra_gpu::projection::MAX_CURVE).into());
    }
    let refine = a.refine.unwrap_or(DEFAULT_REFINE);
    let grey_levels = a.grey_levels.unwrap_or(DEFAULT_GREY_LEVELS);
    if grey_levels > 8 {
        return Err("`grey_levels` is 0 to 8".into());
    }
    let greys: Vec<f64> = (1..=grey_levels).map(|j| f64::from(j) / f64::from(grey_levels + 1)).collect();
    if refine > 10 {
        return Err("`refine` is 0 to 10 rounds".into());
    }
    if let Some(r) = a.area
        && !(r.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) && r[2] > 0.0 && r[3] > 0.0)
    {
        return Err("`area` is [x, y, w, h] within 0..1 of the camera picture".into());
    }
    Ok(Settings { levels, settle, timeout, samples, refine, greys })
}

/// Keeps of `mask` (a `width` x `height` picture's) only the pixels whose centres lie in `area`
/// (`[x, y, w, h]`, 0..1 of the picture).
fn keep_area(mask: &mut [bool], width: usize, height: usize, area: [f64; 4]) {
    let [x, y, w, h] = area;
    let (pw, ph) = (width as f64, height as f64);
    let width = width.max(1);
    for (i, m) in mask.iter_mut().enumerate() {
        let (px, py) = ((i % width) as f64 + 0.5, (i / width) as f64 + 0.5);
        *m &= px >= x * pw && px < (x + w) * pw && py >= y * ph && py < (y + h) * ph;
    }
}

/// The pictures that find where each projector lights the camera's picture (all black, then
/// each one alone in white), with each one's lit pixels.
struct Lit {
    black: Picture,
    whites: Vec<Picture>,
    masks: Vec<Vec<bool>>,
}

/// Takes the [`Lit`] pictures; the masks only within `area`, if given.
fn measure_lit(
    shoot: &mut impl Shoot,
    projectors: &[(NodeId, String)],
    area: Option<[f64; 4]>,
) -> CmdResult<Lit> {
    let black = shoot.shoot(&|_| [0.0; 3])?;
    let mut whites = Vec::with_capacity(projectors.len());
    let mut masks = Vec::with_capacity(projectors.len());
    for (k, (_, path)) in projectors.iter().enumerate() {
        let white = shoot.alone(k, [1.0; 3])?;
        let mut mask = colour::lit(&white, &black).ok_or_else(|| {
            format!("{path} lights no camera pixel (is it in the camera's view, on, and the camera's exposure right?)")
        })?;
        if let Some(area) = area {
            keep_area(&mut mask, black.width, black.height, area);
            if colour::count(&mask) < colour::MIN_PIXELS {
                return Err(format!("{path} lights (almost) nothing inside `area`").into());
            }
        }
        whites.push(white);
        masks.push(mask);
    }
    Ok(Lit { black, whites, masks })
}

/// The ramps' signal levels: `1 / levels` up to 1.
fn level_values(levels: u32) -> Vec<f64> {
    (1..=levels).map(|i| f64::from(i) / f64::from(levels)).collect()
}

/// What the camera read of each projector in its spot: the ramps and white (black subtracted)
/// and the black.
struct Ramps {
    measured: Vec<Measured>,
    /// Per projector its grey at each level, when asked for.
    greys: Vec<Option<Vec<[f64; 3]>>>,
    whites: Vec<Option<[f64; 3]>>,
    blacks: Vec<[f64; 3]>,
}

/// Shows each projector's ramps (each primary at `levels` levels) and reads them in its spot.
fn measure_ramps(
    shoot: &mut impl Shoot,
    projectors: &[(NodeId, String)],
    lit: &Lit,
    spots: &colour::Spots,
    levels: u32,
    with_greys: bool,
    warnings: &mut Vec<String>,
) -> CmdResult<Ramps> {
    let level_values = level_values(levels);
    let mut measured = Vec::with_capacity(projectors.len());
    let mut whites = Vec::with_capacity(projectors.len());
    let mut blacks = Vec::with_capacity(projectors.len());
    let mut greys = Vec::with_capacity(projectors.len());
    for (k, (_, path)) in projectors.iter().enumerate() {
        let spot = spots.masks.get(k).map(Vec::as_slice).unwrap_or(&[]);
        let mean = |p: &Picture| {
            colour::trimmed_mean(p, spot).ok_or_else(|| format!("{path}: an empty measuring spot"))
        };
        let b = mean(&lit.black)?;
        let sub = |v: [f64; 3]| [v[0] - b[0], v[1] - b[1], v[2] - b[2]];
        let w = lit.whites.get(k).map(mean).transpose()?.ok_or("lost a picture")?;
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
                let pic = shoot.alone(k, s)?;
                ramp.push(sub(mean(&pic)?));
            }
        }
        let mut grey = None;
        if with_greys {
            let mut g = Vec::with_capacity(level_values.len());
            for l in &level_values {
                g.push(sub(mean(&shoot.alone(k, [*l as f32; 3])?)?));
            }
            grey = Some(g);
        }
        greys.push(grey);
        blacks.push(b);
        whites.push(Some(sub(w)));
        measured.push(Measured { levels: level_values.clone(), ramps });
    }
    Ok(Ramps { measured, greys, whites, blacks })
}

/// A note of the colour match, its projector named by path.
fn named(note: &colour::Note, projectors: &[(NodeId, String)]) -> String {
    note.named(|k| projectors.get(k).map(|(_, path)| path.clone()))
}

/// What the projectors are compared on: white, the primaries, the greys, the probe.
fn compared_contents(greys: &[f64]) -> Vec<[f64; 3]> {
    let mut contents = vec![WHITE];
    contents.extend(PRIMARIES);
    contents.extend(greys.iter().map(|g| [*g; 3]));
    contents.push([PROBE; 3]);
    contents
}

/// A projector refined against the reference: both spots, its path (for messages) and the
/// most rounds per closed loop.
#[derive(Clone, Copy)]
struct Pair<'s> {
    own: Spot<'s>,
    reference: Spot<'s>,
    path: &'s str,
    rounds: u32,
}

/// The closed loop on white, its outcome.
struct WhiteRounds {
    /// Per round the scales tried and what the camera read.
    rounds: Vec<Json>,
    /// The closest round: its largest log ratio, matrix, log ratios and log scales.
    best: Option<(f64, Mat3, [f64; 3], [f64; 3])>,
    /// The largest log scale each row may take.
    hi: [f64; 3],
    /// What the solver learnt of the camera's slopes.
    solver: colour::Broyden,
}

/// The rows of `base` scaled until the camera reads the projector's white, channel by channel,
/// as the reference's (`want`).
fn refine_white(
    shoot: &mut impl Shoot,
    pair: Pair<'_>,
    base: Mat3,
    curves: &[Vec<f64>; 3],
    want: [f64; 3],
) -> CmdResult<WhiteRounds> {
    let Pair { own, reference, path, rounds: refine } = pair;
    // ln(scale) per row: down to a twentieth, up to a row sum of 1.
    let lo = 0.05_f64.ln();
    let hi = base.map(|row| {
        let sum: f64 = row.iter().sum();
        if sum > 1e-9 { (1.0 / sum).ln().max(lo) } else { 0.0 }
    });
    let mut x = [0.0_f64; 3];
    let mut solver = colour::Broyden::new();
    let mut rounds = Vec::new();
    let mut best: Option<(f64, Mat3, [f64; 3], [f64; 3])> = None;
    for round in 0..=refine {
        let mut m = base;
        for (row, xc) in m.iter_mut().zip(x) {
            *row = row.map(|v| v * xc.exp());
        }
        let got = read_through(shoot, own, &m, curves, &[WHITE])?;
        let got = got.first().copied().ok_or("no reading")?;
        let (f, codes) = compare(got, want, own.black, reference.black).ok_or_else(|| {
            format!(
                "{path}: the camera sees no white in a channel through the correction (or of the reference)"
            )
        })?;
        let err = f.iter().fold(0.0_f64, |a, v| a.max(v.abs()));
        rounds.push(json!({
            "round": round,
            "scale": x.map(f64::exp),
            "ratio": f.map(f64::exp),
            "residual_codes": codes,
            "white": got,
        }));
        if best.as_ref().is_none_or(|(e, _, _, _)| err < *e) {
            best = Some((err, m, f, x));
        }
        if round == refine || err < REFINED {
            break;
        }
        x = solver.step(x, f, lo, hi);
    }
    Ok(WhiteRounds { rounds, best, hi, solver })
}

/// With white matched by the matrix `m`, each channel's curve bent at every grey level until the
/// camera reads that grey as the reference's (`want`, one per grey; one solver per level, the
/// channels interact through the camera; between the levels the bend is interpolated).
fn refine_greys(
    shoot: &mut impl Shoot,
    pair: Pair<'_>,
    m: &Mat3,
    curves: &[Vec<f64>; 3],
    greys: &[f64],
    want: &[[f64; 3]],
    learnt: &colour::Broyden,
) -> CmdResult<GreyRounds> {
    let Pair { own, reference, rounds: refine, .. } = pair;
    let mut grey_rounds = Vec::new();
    let rows = m.map(|row| row.iter().sum::<f64>().clamp(0.0, 1.0));
    let knots: Vec<[f64; 3]> = greys.iter().map(|g| rows.map(|s| colour::encode(s * g))).collect();
    let ends = rows.map(colour::encode);
    let contents: Vec<[f64; 3]> = greys.iter().map(|g| [*g; 3]).collect();
    let mut y = vec![[0.0_f64; 3]; greys.len()];
    // Starting from the slopes white taught: the same camera, the same channels.
    let mut solvers: Vec<colour::Broyden> = greys.iter().map(|_| learnt.restart()).collect();
    let mut best_curves: Option<(f64, [Vec<f64>; 3])> = None;
    let lo = 0.2_f64.ln();
    for round in 0..=refine {
        let now = bend(curves, &knots, ends, &y);
        let got = read_through(shoot, own, m, &now, &contents)?;
        let mut err: f64 = 0.0;
        let mut fs = Vec::with_capacity(greys.len());
        let mut levels = Vec::with_capacity(greys.len());
        for ((g, w), level) in got.iter().zip(want).zip(greys) {
            // A grey too dark to read in a channel is left as it is.
            let f = compare(*g, *w, own.black, reference.black).map_or([0.0; 3], |(f, _)| f);
            err = f.iter().fold(err, |a, v| a.max(v.abs()));
            fs.push(f);
            let mut level_json = ratios(*g, *w, own.black, reference.black);
            if let Some(o) = level_json.as_object_mut() {
                o.insert("grey".into(), json!(level));
            }
            levels.push(level_json);
        }
        grey_rounds.push(json!({"round": round, "levels": levels}));
        if best_curves.as_ref().is_none_or(|(e, _)| err < *e) {
            best_curves = Some((err, now));
        }
        if round == refine || err < REFINED {
            break;
        }
        for ((yj, f), (solver, kj)) in y.iter_mut().zip(fs).zip(solvers.iter_mut().zip(&knots)) {
            // At most full signal where the bend peaks.
            let mut hi = [0.0; 3];
            for ((h, curve), e) in hi.iter_mut().zip(curves).zip(kj) {
                *h = (1.0 / colour::lookup(curve, *e).max(1e-3)).ln().max(lo);
            }
            *yj = solver.step(*yj, f, lo, hi);
        }
    }
    Ok(GreyRounds { rounds: grey_rounds, best: best_curves.map(|(_, c)| c) })
}

/// The closed loop on the greys, its outcome: per round what the camera read, and the closest
/// curves.
struct GreyRounds {
    rounds: Vec<Json>,
    best: Option<[Vec<f64>; 3]>,
}

/// One projector refined against the reference.
struct Refined {
    /// The matrix of the closest white round (none without a round).
    matrix: Option<Mat3>,
    curves: [Vec<f64>; 3],
    report: Json,
    warnings: Vec<String>,
}

/// Refines the pair's projector (`base` matrix, `curves`) against the reference's readings
/// `want_all` (of [`compared_contents`]): the matrix on white, then the curves on the greys;
/// then reads the primaries, the greys and the probe through the result, for the report.
fn refine_projector(
    shoot: &mut impl Shoot,
    pair: Pair<'_>,
    base: Mat3,
    curves: &[Vec<f64>; 3],
    want_all: &[[f64; 3]],
    greys: &[f64],
) -> CmdResult<Refined> {
    let want = want_all.first().copied().ok_or("no reference reading")?;
    let white = refine_white(shoot, pair, base, curves, want)?;
    let m_final = white.best.as_ref().map_or(base, |b| b.1);
    let mut final_curves = curves.clone();
    let mut grey_rounds = Vec::new();
    let want_greys: Vec<[f64; 3]> =
        want_all.iter().skip(1 + PRIMARIES.len()).take(greys.len()).copied().collect();
    if !greys.is_empty() && want_greys.len() == greys.len() {
        let grey_fit = refine_greys(shoot, pair, &m_final, curves, greys, &want_greys, &white.solver)?;
        grey_rounds = grey_fit.rounds;
        if let Some(c) = grey_fit.best {
            final_curves = c;
        }
    }

    // Reported, not adjusted: the primaries, the greys and a grey between them, through the
    // result.
    let mut check = serde_json::Map::new();
    let mut names: Vec<String> = NAMES.iter().map(|n| (*n).to_string()).collect();
    names.extend(greys.iter().chain([PROBE].iter()).map(|g| format!("grey {g}")));
    let contents: Vec<[f64; 3]> = compared_contents(greys).into_iter().skip(1).collect();
    let got = read_through(shoot, pair.own, &m_final, &final_curves, &contents)?;
    for ((name, g), w) in names.iter().zip(&got).zip(want_all.iter().skip(1)) {
        check.insert(name.clone(), ratios(*g, *w, pair.own.black, pair.reference.black));
    }
    let mut warnings = Vec::new();
    let mut matrix = None;
    if let Some((_, m, f, x)) = white.best {
        warnings = white_limits(pair.path, f, x, white.hi);
        matrix = Some(m);
    }
    let report =
        json!({"reference_white": want, "rounds": white.rounds, "grey_rounds": grey_rounds, "check": check});
    Ok(Refined { matrix, curves: final_curves, report, warnings })
}

/// Warnings for the channels whose white stayed dimmer than the reference's (log ratio `f`)
/// with their row's log scale `x` at its limit `hi`.
fn white_limits(path: &str, f: [f64; 3], x: [f64; 3], hi: [f64; 3]) -> Vec<String> {
    let mut out = Vec::new();
    for ((name, fc), (xc, hc)) in NAMES.iter().zip(f).zip(x.into_iter().zip(hi)) {
        if fc < -0.02 && (xc - hc).abs() < 1e-9 {
            out.push(format!(
                "{path}: cannot show the {name} of white as bright as the reference ({:.0} %); lower the reference's gain",
                fc.exp() * 100.0
            ));
        }
    }
    out
}

/// The params to write (matrix, curves, correction on) and the report, per projector.
fn results(
    projectors: &[(NodeId, String)],
    matched: &colour::Matched,
    ramps: &Ramps,
    matrices: &[Mat3],
    curve_sets: &[[Vec<f64>; 3]],
    refined: &[Json],
) -> CmdResult<(Vec<Op>, Vec<Json>)> {
    let mut ops = Vec::new();
    let mut report = Vec::new();
    for (k, ((_, path), c)) in projectors.iter().zip(&matched.projectors).enumerate() {
        let refined_matrix = matrices.get(k).copied().unwrap_or(c.matrix);
        let peak = [1.0, 0.5].map(|s| colour::peak(&c.primaries, &colour::blend_matrix(&refined_matrix, s)));
        let matrix = correct::matrix_text(&refined_matrix);
        let curves = correct::curves_text(curve_sets.get(k).unwrap_or(&c.curves));
        let node = NodePath::parse(path)?;
        for (name, value) in [
            ("matrix", Value::Text(matrix.clone())),
            ("curves", Value::Text(curves.clone())),
            ("correct", Value::Bool(true)),
        ] {
            let param = ParamPath::new(node.clone(), Name::new(name)?)?;
            ops.push(Op::Set { param, value });
        }
        let m = ramps.measured.get(k);
        report.push(json!({
            "path": path,
            "black": ramps.blacks.get(k),
            "white": ramps.whites.get(k),
            "primaries": c.primaries,
            "levels": m.map(|m| &m.levels),
            "ramps": m.map(|m| &m.ramps),
            "response": c.response,
            "additivity": c.additivity,
            "peak": {"strength_1": peak[0], "strength_0_5": peak[1]},
            "unrefined_matrix": correct::matrix_text(&c.matrix),
            "refine": refined.get(k),
            "matrix": matrix,
            "curves": curves,
        }));
    }
    Ok((ops, report))
}

/// `projector.match` in stages: the settings; the projectors and the camera; where each lights
/// the camera ([`measure_lit`]); the ramps ([`measure_ramps`]) and the correction they give; the
/// closed loop per projector ([`refine_projector`]); the params and the report ([`results`]),
/// written as one undo step.
pub fn run(e: &Engine, a: &MatchArgs) -> CmdResult<Json> {
    let started = Instant::now();
    let Settings { levels, settle, timeout, samples, refine, greys } = settings(a)?;

    let Some(camera_path) = &a.camera else {
        return Err(
            "give `camera` (a tex.video_in) to measure, or `from` to match stored measurements".into()
        );
    };
    let session = e.session();
    let net = session.doc().snapshot();
    let projectors = projector_list(e, &net, a.projectors.as_deref())?;
    if projectors.is_empty() {
        return Err(format!("no active {PROJECTOR} to measure").into());
    }
    if let Some(dir) = &a.save {
        if dir.trim().is_empty() {
            return Err("`save` is a folder, relative to the project".into());
        }
        // Names checked before anything is shown.
        let mut names = Vec::new();
        for (_, path) in &projectors {
            let name = stored_files::file_name(path)?;
            if names.contains(&name) {
                return Err(format!("two projectors would both be saved as {name}").into());
            }
            names.push(name);
        }
    }
    let camera = CameraStream::open(e, &net, camera_path)?;
    let camera_of = CameraOf {
        node: camera_path.to_string(),
        device: camera.key.device.clone(),
        mode: camera.key.mode.clone(),
    };
    let claim = Claim::new(e, projectors.iter().map(|(id, _)| *id).collect())?;
    let mut rig = Rig {
        engine: e,
        projectors: projectors.clone(),
        camera,
        settle: Duration::from_secs_f64(settle),
        timeout: Duration::from_secs_f64(timeout),
        pass: e.output().pass,
        frames: 1,
    };

    // Where each projector lights the camera picture.
    let lit = measure_lit(&mut rig, &projectors, a.area)?;
    let spots = colour::spots(&lit.masks, lit.black.width, lit.black.height);
    let mut warnings = Vec::new();
    if projectors.len() > 1 && !spots.shared {
        warnings.push(
            "the projectors do not overlap in the camera's view; differences of the wall and the camera's \
             vignetting count as projector differences (each was measured on its own part of the wall)"
                .to_string(),
        );
    }

    // The ramps, and the correction they give.
    let ramps = measure_ramps(&mut rig, &projectors, &lit, &spots, levels, a.save.is_some(), &mut warnings)?;
    let matched = colour::match_projectors(&ramps.measured, &ramps.whites, samples, a.target)
        .map_err(|note| named(&note, &projectors))?;
    warnings.extend(matched.warnings.iter().map(|w| named(w, &projectors)));
    let whites: Vec<[f64; 3]> = ramps.whites.iter().flatten().copied().collect();
    if let Some(reference_white) = ramps.whites.get(matched.reference).copied().flatten() {
        let gain = colour::stored::between_gain(&whites, reference_white, matched.alpha);
        if gain > 1.02 {
            warnings.push(format!(
                "no projector is the dimmest in every channel: a white between the white points would keep \
                 about {:.0} % more brightness (the camera's estimate); run with save, then match from the \
                 saved measurements",
                (gain - 1.0) * 100.0
            ));
        }
    }

    // Closed loop: each other projector's matrix rows scaled until the camera reads its white,
    // channel by channel, as it reads the reference's. White and not the primaries one by one:
    // cameras do not add up (white reads less than red + green + blue), so matched primaries
    // leave white off.
    let mut matrices: Vec<Mat3> = matched.projectors.iter().map(|c| c.matrix).collect();
    let mut curve_sets: Vec<[Vec<f64>; 3]> = matched.projectors.iter().map(|c| c.curves.clone()).collect();
    let mut refined: Vec<Json> = vec![Json::Null; projectors.len()];
    let r = matched.reference;
    if refine > 0 && projectors.len() > 1 {
        let spot = |k: usize| Spot {
            k,
            mask: spots.masks.get(k).map(Vec::as_slice).unwrap_or(&[]),
            black: ramps.blacks.get(k).copied().unwrap_or([0.0; 3]),
        };
        let curves_of = |k: usize| matched.projectors.get(k).map(|c| c.curves.clone()).unwrap_or_default();
        let ref_matrix = matrices.get(r).copied().ok_or("no reference")?;
        let want_all =
            read_through(&mut rig, spot(r), &ref_matrix, &curves_of(r), &compared_contents(&greys))?;
        if want_all.is_empty() {
            return Err("no reference reading".into());
        }
        for (k, (_, path)) in projectors.iter().enumerate() {
            if k == r {
                continue;
            }
            let base = matrices.get(k).copied().ok_or("lost a matrix")?;
            let pair = Pair { own: spot(k), reference: spot(r), path, rounds: refine };
            let done = refine_projector(&mut rig, pair, base, &curves_of(k), &want_all, &greys)?;
            if let Some(slot) = curve_sets.get_mut(k) {
                *slot = done.curves;
            }
            warnings.extend(done.warnings);
            if let (Some(m), Some(slot)) = (done.matrix, matrices.get_mut(k)) {
                *slot = m;
            }
            if let Some(slot) = refined.get_mut(k) {
                *slot = done.report;
            }
        }
    }
    drop(claim);

    let mut saved = Json::Null;
    if let Some(dir) = &a.save {
        let (run_id, unix_time) = stored_files::run_id();
        let info = RunInfo {
            run: &run_id,
            unix_time,
            camera: camera_of,
            shared_spot: spots.shared,
            note: a.note.as_deref().unwrap_or(""),
        };
        let mut files = Vec::with_capacity(projectors.len());
        let mut stored_list = Vec::with_capacity(projectors.len());
        for (k, (id, path)) in projectors.iter().enumerate() {
            let m = ramps.measured.get(k).ok_or("lost a measurement")?;
            let greys = ramps.greys.get(k).cloned().flatten();
            stored_list.push(colour::stored::Stored { measured: m.clone(), greys: greys.clone() });
            let file = stored_files::MeasurementFile {
                format: stored_files::FORMAT.into(),
                version: stored_files::VERSION,
                projector: path.clone(),
                display: text_param(&net, e, *id, PROJECTOR, "display"),
                note: info.note.into(),
                run: info.run.into(),
                unix_time: info.unix_time,
                camera: info.camera.clone(),
                spot_pixels: spots.masks.get(k).map_or(0, |m| colour::count(m)),
                shared_spot: info.shared_spot,
                levels: m.levels.clone(),
                black: ramps.blacks.get(k).copied().unwrap_or([0.0; 3]),
                white: ramps.whites.get(k).copied().flatten(),
                ramps: m.ramps.clone(),
                greys,
            };
            files.push((path.clone(), file));
        }
        let written = stored_files::save(&session.dir().join(dir), &files)?;
        let check = stored_files::check_against(&stored_list, &projectors, &matrices, &curve_sets, samples);
        saved = json!({"files": written, "run": run_id, "stored_match_against_this": check});
    }

    let (ops, report) = results(&projectors, &matched, &ramps, &matrices, &curve_sets, &refined)?;
    let apply = a.apply.unwrap_or(true);
    let history = if apply { Some(outcome(&edit_in(&session, &ops, "projector.match")?)) } else { None };
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
        "target": a.target,
        "shared_spot": spots.shared,
        "spot_pixels": spots.masks.iter().map(|m| colour::count(m)).collect::<Vec<_>>(),
        "camera_picture": [lit.black.width, lit.black.height],
        "projectors": report,
        "applied": apply,
        "history": history,
        "saved": saved,
        "warnings": warnings,
        "seconds": started.elapsed().as_secs_f64(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A camera whose 16 x 16 picture every projector lights evenly: per channel `gain` times
    /// the linear light of its signal, over the room's `black`.
    struct Even {
        gains: Vec<[f64; 3]>,
        black: f64,
    }

    impl Shoot for Even {
        fn shoot(&mut self, signal: &dyn Fn(usize) -> [f32; 3]) -> CmdResult<Picture> {
            let mut v = [self.black; 3];
            for (k, g) in self.gains.iter().enumerate() {
                let s = signal(k);
                for c in 0..3 {
                    v[c] += g[c] * colour::decode(f64::from(s[c]));
                }
            }
            Ok(Picture { width: 16, height: 16, rgb: vec![v.map(|x| x as f32); 256] })
        }
    }

    fn straight() -> [Vec<f64>; 3] {
        let c: Vec<f64> = (0..17).map(|i| f64::from(i) / 16.0).collect();
        [c.clone(), c.clone(), c]
    }

    fn diag(v: f64) -> Mat3 {
        [[v, 0.0, 0.0], [0.0, v, 0.0], [0.0, 0.0, v]]
    }

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    fn two() -> Vec<(NodeId, String)> {
        vec![(NodeId(1), "/a".into()), (NodeId(2), "/b".into())]
    }

    fn args(v: Json) -> MatchArgs {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn settings_fill_defaults_and_check_ranges() {
        let s = settings(&args(json!({"camera": "/cam"}))).unwrap();
        assert_eq!((s.levels, s.samples, s.refine), (DEFAULT_LEVELS, DEFAULT_SAMPLES, DEFAULT_REFINE));
        assert_eq!(s.greys, [0.25, 0.5, 0.75]);
        assert!(settings(&args(json!({"camera": "/cam", "grey_levels": 0}))).unwrap().greys.is_empty());
        for (bad, says) in [
            (json!({"levels": 1}), "`levels`"),
            (json!({"grey_levels": 9}), "`grey_levels`"),
            (json!({"refine": 11}), "`refine`"),
            (json!({"settle": 11.0}), "`settle`"),
            (json!({"samples": 1}), "`samples`"),
            (json!({"area": [0.0, 0.0, 0.0, 1.0]}), "`area`"),
            (json!({"area": [0.0, 0.0, 1.5, 1.0]}), "`area`"),
        ] {
            let mut v = bad.clone();
            v["camera"] = json!("/cam");
            let err = settings(&args(v)).unwrap_err();
            assert!(err.0.contains(says), "{bad}: {}", err.0);
        }
    }

    #[test]
    fn bend_leaves_white_and_unbent_curves_alone() {
        let curves = straight();
        let knots = [[0.5; 3]];
        assert_eq!(bend(&curves, &knots, [1.0; 3], &[[0.0; 3]]), curves);
        // Raised at the knot, unchanged from white's sample on and at black.
        let bent = bend(&curves, &knots, [0.8; 3], &[[0.2, 0.0, -0.2]]);
        assert!(bent[0][8] > curves[0][8] && bent[2][8] < curves[2][8], "{bent:?}");
        assert_eq!(bent[1], curves[1]);
        for c in &bent {
            assert_eq!(c[0], 0.0);
            assert_eq!(&c[12..], &curves[0][12..]);
        }
        // A knot at or beyond white's sample is ignored.
        assert_eq!(bend(&curves, &[[0.9; 3]], [0.8; 3], &[[0.5; 3]]), curves);
    }

    #[test]
    fn ratios_and_compare_read_channel_by_channel() {
        let r = ratios([0.4, 0.2, 0.0], [0.2, 0.2, 0.1], [0.0; 3], [0.0; 3]);
        assert_eq!(r["ratio"], json!([2.0, 1.0, null]));
        let codes = r["residual_codes"].as_array().unwrap();
        assert!(codes[0].as_f64().unwrap() > 0.0 && codes[1].as_f64().unwrap() == 0.0);
        let (f, codes) = compare([0.4, 0.2, 0.1], [0.2, 0.2, 0.2], [0.01; 3], [0.01; 3]).unwrap();
        assert!(close(f[0], 2f64.ln(), 1e-12) && f[1] == 0.0 && close(f[2], 0.5f64.ln(), 1e-12));
        assert!(codes[0] > 0.0 && codes[1] == 0.0 && codes[2] < 0.0);
        assert_eq!(compare([0.4, 0.0, 0.1], [0.2; 3], [0.0; 3], [0.0; 3]), None);
    }

    #[test]
    fn keep_area_keeps_pixel_centres_inside() {
        let mut mask = vec![true; 8];
        keep_area(&mut mask, 4, 2, [0.5, 0.0, 0.5, 0.5]);
        assert_eq!(mask, [false, false, true, true, false, false, false, false]);
        let mut empty: Vec<bool> = Vec::new();
        keep_area(&mut empty, 0, 0, [0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn levels_and_contents() {
        assert_eq!(level_values(4), [0.25, 0.5, 0.75, 1.0]);
        let c = compared_contents(&[0.5]);
        assert_eq!(c, [WHITE, PRIMARIES[0], PRIMARIES[1], PRIMARIES[2], [0.5; 3], [PROBE; 3]]);
    }

    #[test]
    fn notes_name_projectors_by_path() {
        let err = colour::match_projectors(&[], &[], 17, Target::Reference).unwrap_err();
        assert_eq!(named(&err, &two()), "no projectors to match");
        let note = colour::Note { projector: Some(1), text: "dim".into() };
        assert_eq!(named(&note, &two()), "/b: dim");
        let far = colour::Note { projector: Some(5), text: "dim".into() };
        assert_eq!(named(&far, &two()), "projector 5: dim");
    }

    #[test]
    fn white_limits_warn_only_at_the_limit() {
        let w = white_limits("/b", [-0.1, -0.1, 0.0], [0.3, 0.2, 0.3], [0.3, 0.3, 0.3]);
        assert_eq!(w.len(), 1);
        assert!(w[0].starts_with("/b: cannot show the red of white") && w[0].contains("(90 %)"), "{w:?}");
    }

    #[test]
    fn measuring_reads_the_ramps_in_each_spot() {
        let mut cam = Even { gains: vec![[0.5, 0.4, 0.3], [1.0, 0.6, 0.6]], black: 0.01 };
        let projectors = two();
        let lit = measure_lit(&mut cam, &projectors, None).unwrap();
        assert_eq!(lit.whites.len(), 2);
        let spots = colour::spots(&lit.masks, lit.black.width, lit.black.height);
        assert!(spots.shared);
        let mut warnings = Vec::new();
        let ramps = measure_ramps(&mut cam, &projectors, &lit, &spots, 4, false, &mut warnings).unwrap();
        // Projector 1's white reads 1.01 in red: clipped.
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].starts_with("/b: the camera clips"), "{warnings:?}");
        assert!(close(ramps.blacks[0][0], 0.01, 1e-6));
        let red = &ramps.measured[0].ramps[0];
        for (l, v) in level_values(4).iter().zip(red) {
            assert!(close(v[0], 0.5 * colour::decode(*l), 1e-6) && close(v[1], 0.0, 1e-6), "{red:?}");
        }
        assert!(close(ramps.whites[1].unwrap()[1], 0.6, 1e-6));

        // Through the colour match to the params and the report.
        let matched =
            colour::match_projectors(&ramps.measured, &ramps.whites, 17, Target::Reference).unwrap();
        let matrices: Vec<Mat3> = matched.projectors.iter().map(|c| c.matrix).collect();
        let curves: Vec<[Vec<f64>; 3]> = matched.projectors.iter().map(|c| c.curves.clone()).collect();
        let (ops, report) =
            results(&projectors, &matched, &ramps, &matrices, &curves, &[Json::Null, Json::Null]).unwrap();
        assert_eq!(ops.len(), 6);
        assert_eq!(report[1]["path"], "/b");
        assert_eq!(report[0]["levels"], json!([0.25, 0.5, 0.75, 1.0]));
        // The dimmer projector is the reference; the brighter one is turned down to it.
        assert_eq!(matched.reference, 0);
        assert!(close(matrices[1][0][0], 0.5, 1e-3), "{:?}", matrices[1]);
    }

    #[test]
    fn measuring_fails_on_no_light_or_a_too_small_area() {
        let mut cam = Even { gains: vec![[0.5; 3]], black: 0.0 };
        let projectors = vec![(NodeId(1), "/a".to_string())];
        assert!(measure_lit(&mut cam, &projectors, Some([0.0, 0.0, 0.5, 0.5])).is_ok());
        let err = measure_lit(&mut cam, &projectors, Some([0.0, 0.0, 0.1, 0.1])).err().unwrap();
        assert!(err.0.contains("inside `area`"), "{}", err.0);
        let mut dark = Even { gains: vec![[0.0; 3]], black: 0.1 };
        assert!(
            measure_lit(&mut dark, &projectors, None).err().unwrap().0.contains("lights no camera pixel")
        );
    }

    /// Projector 1 refined against projector 0 by an [`Even`] camera.
    fn refine_against(gains: Vec<[f64; 3]>, base: Mat3) -> Refined {
        let mut cam = Even { gains, black: 0.0 };
        let mask = vec![true; 256];
        let spot = |k| Spot { k, mask: &mask, black: [0.0; 3] };
        let greys = [0.25, 0.5, 0.75];
        let want_all =
            read_through(&mut cam, spot(0), &diag(1.0), &straight(), &compared_contents(&greys)).unwrap();
        let pair = Pair { own: spot(1), reference: spot(0), path: "/b", rounds: 5 };
        refine_projector(&mut cam, pair, base, &straight(), &want_all, &greys).unwrap()
    }

    #[test]
    fn refining_scales_white_to_the_reference() {
        // The reference reads 0.8 of white; the other, at half drive, 0.5 of its 1.
        let done = refine_against(vec![[0.8; 3], [1.0; 3]], diag(0.5));
        let m = done.matrix.unwrap();
        for c in 0..3 {
            assert!(close(m[c][c], 0.8, 0.8 * REFINED), "{m:?}");
        }
        assert!(done.warnings.is_empty(), "{:?}", done.warnings);
        let rounds = done.report["rounds"].as_array().unwrap();
        assert_eq!(rounds.len(), 2, "{rounds:?}");
        // A linear camera: the greys match with the matrix, the curves stay.
        assert_eq!(done.report["grey_rounds"].as_array().unwrap().len(), 1);
        assert_eq!(done.curves, straight());
        let check = done.report["check"].as_object().unwrap();
        assert_eq!(check.len(), 3 + 3 + 1);
        let ratio = check["grey 0.375"]["ratio"][1].as_f64().unwrap();
        assert!(close(ratio, 1.0, 0.01), "{ratio}");
    }

    #[test]
    fn refining_warns_when_the_projector_cannot_reach_the_reference() {
        let done = refine_against(vec![[0.8; 3], [0.5; 3]], diag(1.0));
        assert_eq!(done.matrix, Some(diag(1.0)));
        assert_eq!(done.warnings.len(), 3, "{:?}", done.warnings);
        assert!(done.warnings[1].contains("green of white") && done.warnings[1].contains("(62 %)"));
    }
}
