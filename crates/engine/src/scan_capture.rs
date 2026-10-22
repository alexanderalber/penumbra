//! `scan.capture`: structured light scans of projectors, live, with a camera node.
//!
//! One projector at a time shows the scan's patterns while the others show black, through the
//! output board ([`penumbra_ops::out::BoardState::stripes`] and `flat`) as `projector.match`
//! shows its colours: the network is not edited, the patterns bypass the projector's colour
//! correction (a dimmed projector would lose contrast and pixels), and everything shown is
//! cleared when the command ends, however it ends. The stripes are drawn in the projector's own
//! pixels, whatever feeds it.
//!
//! Each picture is taken once the change reached the projector (the cook thread published it
//! and every output window presented it), the light-to-camera latency (plus a camera frame) and
//! `settle` passed, the camera's picture changed from the one taken before, and the camera then
//! delivered two frames that agree (`verify`); the first frame that changed is never taken, it
//! may have caught the change half way. The latency is measured on the way: the white pattern
//! follows black and the black pattern white, and the first camera frame brighter (darker) than
//! before tells when the light arrived. A change seen earlier than half the latency is not
//! counted (fine stripes flicker in the camera, and the pattern before may still be arriving), and
//! a change alone never lets a frame from before the latency through. Stripes finer than the
//! camera resolves change nothing it can see: those are taken once twice the latency passed. Pattern and inverse of the coarsest bits are checked to differ
//! clearly where the projector reaches, and taken again once if they do not.
//!
//! The camera frames go straight into the streaming decoder (no files in between, unless
//! `frames` keeps them), and the decoded maps are written as `scan.decode` writes them, one folder
//! per projector. Like `projector.match`, the command runs on the caller's thread and only waits
//! there: the cook thread keeps cooking and the outputs keep presenting.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use penumbra_calib::colour;
use penumbra_calib::scan::{Decoder, Options, Pattern, Patterns};
use penumbra_camera::CameraFrame;
use penumbra_core::{NodeId, NodePath, Value};
use penumbra_gpu::Pixels;
use penumbra_gpu::projection::Stripes;
use penumbra_graph::Network;
use penumbra_ops::out::{PROJECTOR, raster};
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, FnCommand, Registry, args};
use crate::engine::Engine;
use crate::projector::{CameraStream, Claim, projector_list, settle_of, timeout_of, wait_shown};
use crate::scan::{Decoded, fill_of, ms, write_decoded};

type CmdResult<T> = std::result::Result<T, CommandError>;

/// Where `scan.capture` writes unless told otherwise (relative to the project): a folder per
/// projector inside.
pub const DEFAULT_OUT: &str = "scan";
/// The folder inside a projector's scan folder that keeps the camera frames (`frames: true`).
pub const FRAMES: &str = "frames";
pub const DEFAULT_SETTLE: f64 = 0.05;
pub const DEFAULT_RETRIES: u32 = 3;
/// How long the black before the first projector's white is waited for, before any latency is
/// known.
const FIRST_WAIT: Duration = Duration::from_millis(500);
/// Every this many camera pixels across and down are compared between frames.
const SAMPLE: usize = 4;
/// Luma codes (8-bit) by which a sample must differ to count as changed.
const CHANGED: i16 = 12;
/// The share of all samples that must change for the picture to count as changed (white after
/// black, black after white).
const CHANGE_SHARE: f64 = 0.005;
/// Two frames agree when at most this share of the samples changed: of those the projector
/// reaches once that is known, else of all.
const UNSTABLE_SHARE: f64 = 0.02;
/// Least median margin (pattern minus inverse over white minus black) of a checked bit.
const MIN_MARGIN: f64 = 0.35;
/// The coarsest bits of each axis whose pattern and inverse are checked.
const CHECKED_BITS: u32 = 2;

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let e = Arc::clone(engine);
    registry.register(FnCommand::new(
        "scan.capture",
        "Scan projectors with a camera, live. Each projector alone shows the structured light patterns \
         (in its own pixels, past its colour correction, the network untouched) while the others show \
         black. The camera's frames are decoded and written as scan.decode writes them, into \
         out/<projector> (the path without its leading slash, slashes as _). Measures the \
         light-to-camera latency on the way. Blocks about 15 s per projector; {camera (tex.video_in \
         path), projectors? (paths, default every \
         active out.projector), out? (folder, scan), settle? (s after the latency, 0.05), timeout? (s, 5), \
         level? (signal of the lit stripes, 1), frames? (keep the camera frames as PNG in \
         out/<projector>/frames, false), verify? (take a picture only when two frames agree, true), \
         retries? (frames that may disagree, 3), fill? (projector px, 32), options?: {min_contrast?, \
         saturation?, min_confidence?, tolerance?}}",
        move |p| {
            let a: CaptureArgs = args(p)?;
            capture(&e, &a)
        },
    ));
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureArgs {
    pub camera: NodePath,
    pub projectors: Option<Vec<NodePath>>,
    pub out: Option<PathBuf>,
    pub settle: Option<f64>,
    pub timeout: Option<f64>,
    pub level: Option<f32>,
    pub frames: Option<bool>,
    pub verify: Option<bool>,
    pub retries: Option<u32>,
    pub fill: Option<u32>,
    #[serde(default)]
    pub options: Options,
}

pub use penumbra_ops::tex::scan::folder_name;

/// How a caller follows a capture running on its thread: a flag that stops it (checked before
/// each picture) and a report of the share done, 0 to 1, per projector in turn.
pub struct Watch<'a> {
    pub cancel: &'a AtomicBool,
    pub progress: &'a dyn Fn(usize, f32),
}

/// What a cancelled capture fails with.
pub const CANCELLED: &str = "cancelled";

/// Every [`SAMPLE`]-th luma code across and down.
fn luma(f: &CameraFrame) -> Vec<u8> {
    let (w, h) = (f.width as usize, f.height as usize);
    let mut out = Vec::with_capacity(w.div_ceil(SAMPLE) * h.div_ceil(SAMPLE));
    for y in (0..h).step_by(SAMPLE) {
        for x in (0..w).step_by(SAMPLE) {
            out.push(f.y.get(y * w + x).copied().unwrap_or(0));
        }
    }
    out
}

/// Which way a change counts.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Way {
    Any,
    Brighter,
    Darker,
}

/// Samples changed by more than [`CHANGED`] (the way asked for) from `a` to `b`, and samples
/// compared (those of `mask`, or all).
fn changed(a: &[u8], b: &[u8], mask: Option<&[bool]>, way: Way) -> (usize, usize) {
    let mut count = 0;
    let mut total = 0;
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        if mask.is_some_and(|m| !m.get(i).copied().unwrap_or(false)) {
            continue;
        }
        total += 1;
        let d = i16::from(*y) - i16::from(*x);
        let hit = match way {
            Way::Any => d.abs() > CHANGED,
            Way::Brighter => d > CHANGED,
            Way::Darker => -d > CHANGED,
        };
        if hit {
            count += 1;
        }
    }
    (count, total)
}

/// The median of `pattern` minus `inverse` over white minus black, where the projector reaches.
fn margin(white: &[u8], black: &[u8], pattern: &[u8], inverse: &[u8]) -> Option<f64> {
    let mut m: Vec<f64> = Vec::new();
    for (((w, b), p), q) in white.iter().zip(black).zip(pattern).zip(inverse) {
        let range = i16::from(*w) - i16::from(*b);
        if range > CHANGED {
            m.push(f64::from((i16::from(*p) - i16::from(*q)).abs()) / f64::from(range));
        }
    }
    if m.is_empty() {
        return None;
    }
    m.sort_by(f64::total_cmp);
    m.get(m.len() / 2).copied()
}

/// A camera picture taken.
struct Shot {
    frame: Arc<CameraFrame>,
    luma: Vec<u8>,
    /// When the first frame differing from the picture before arrived (asked for only).
    changed_at: Option<Instant>,
    /// When the change was seen presented.
    presented: Instant,
    /// Frames that disagreed with the next before this one.
    unstable: u32,
    /// Still disagreeing after the retries.
    settled: bool,
}

struct Scanner<'a> {
    e: &'a Engine,
    /// Every projector shown: those scanned and every other active one (black throughout).
    projectors: Vec<(NodeId, String)>,
    camera: CameraStream,
    settle: Duration,
    timeout: Duration,
    verify: bool,
    retries: u32,
    level: f32,
    pass: u64,
    /// Time between consecutive camera frames, as seen.
    intervals: Vec<Duration>,
    watch: Option<&'a Watch<'a>>,
}

impl Scanner<'_> {
    /// Shows `pattern` of a `size` projector on projector `lit`, black on every other active
    /// one, and waits until all are presented.
    fn show(&mut self, lit: NodeId, pattern: usize, size: (u32, u32)) -> CmdResult<Instant> {
        if self.watch.is_some_and(|w| w.cancel.load(Ordering::Relaxed)) {
            return Err(CANCELLED.into());
        }
        let stripes = Stripes {
            pattern: u32::try_from(pattern).unwrap_or(u32::MAX),
            level: self.level,
            size: [size.0, size.1],
        };
        self.e.board().update(|b| {
            for (id, _) in &self.projectors {
                if *id == lit {
                    b.flat.remove(id);
                    b.stripes.insert(*id, stripes);
                } else {
                    b.stripes.remove(id);
                    b.flat.insert(*id, [0.0; 3]);
                }
            }
        });
        wait_shown(self.e, &self.projectors, &mut self.pass, self.timeout, |p| {
            if p.node == lit {
                p.flat.is_none() && p.stripes == Some(stripes)
            } else {
                p.flat == Some([0.0; 3])
            }
        })
    }

    /// The camera's picture of what was presented at `presented`: the first frame at least
    /// `latency` plus `settle` later (and, with `change_from`, after the first frame that is
    /// brighter or darker than that picture, as asked), then frames until two agree (where `lit`,
    /// if given). A change the other way is something else leaving the picture (the projector
    /// scanned before, the content before the scan), not the light asked for.
    fn take(
        &mut self,
        what: &str,
        presented: Instant,
        latency: Duration,
        change_from: Option<(&[u8], Way)>,
        lit: Option<&[bool]>,
        fallback: Option<Duration>,
    ) -> CmdResult<Shot> {
        let gate = presented + latency + self.settle;
        let fallback = fallback.map(|d| gate + d);
        let deadline = gate + self.timeout;
        let mut seen = 0u64;
        let mut last: Option<Arc<CameraFrame>> = None;
        let mut changed_at: Option<Instant> = None;
        // The first of the frames changed in a row so far.
        let mut candidate: Option<Instant> = None;
        // The frame that confirmed the change.
        let mut confirmed = presented;
        let (mut frame, mut l) = loop {
            let f = self.camera.next(self.e, self.timeout, |f| f.sequence != seen)?;
            if let Some(last) = &last
                && f.sequence == last.sequence + 1
                && self.intervals.len() < 64
            {
                self.intervals.push(f.arrived.saturating_duration_since(last.arrived));
            }
            seen = f.sequence;
            last = Some(f.clone());
            if Instant::now() > deadline {
                return Err(match change_from {
                    Some(_) if changed_at.is_none() => format!(
                        "{what}: the camera's picture did not change (is the projector in the camera's view, on, \
                         and the exposure right?)"
                    ),
                    _ => format!("{what}: no camera frame in time"),
                }
                .into());
            }
            // A frame from before half the latency cannot show the new light: changes in it are
            // the old picture still arriving, or the camera's flicker on fine stripes.
            if f.arrived <= presented + latency / 2 {
                continue;
            }
            let l = luma(&f);
            if let Some((from, way)) = change_from {
                match changed_at {
                    None => {
                        let (c, n) = changed(from, &l, lit, way);
                        let share = if lit.is_some() { UNSTABLE_SHARE } else { CHANGE_SHARE };
                        if n > 0 && c as f64 > share * n as f64 {
                            // A change counts once the next frame shows it too: a single frame is
                            // the camera's flicker. Its first frame tells when the light arrived;
                            // neither is taken, the first may have caught the change half way.
                            match candidate {
                                Some(first) => {
                                    changed_at = Some(first);
                                    confirmed = f.arrived;
                                }
                                None => candidate = Some(f.arrived),
                            }
                            continue;
                        }
                        candidate = None;
                        // Patterns finer than the camera resolves look alike: no change to wait
                        // for once the light has surely arrived.
                        if !fallback.is_some_and(|t| f.arrived >= t) {
                            continue;
                        }
                    }
                    Some(_) if f.arrived <= confirmed => continue,
                    Some(_) => {}
                }
            }
            if f.arrived >= gate {
                break (f, l);
            }
        };
        let mut unstable = 0;
        let mut settled = true;
        if self.verify {
            loop {
                let sequence = frame.sequence;
                let next = self.camera.next(self.e, self.timeout, |f| f.sequence != sequence)?;
                let ln = luma(&next);
                let (c, n) = changed(&l, &ln, lit, Way::Any);
                frame = next;
                l = ln;
                if c as f64 <= UNSTABLE_SHARE * n.max(1) as f64 {
                    break;
                }
                unstable += 1;
                if unstable > self.retries {
                    settled = false;
                    break;
                }
            }
        }
        Ok(Shot { frame, luma: l, changed_at, presented, unstable, settled })
    }

    /// The camera's usual time between frames (the median seen), zero before any.
    fn interval(&self) -> Duration {
        let mut v = self.intervals.clone();
        v.sort();
        v.get(v.len() / 2).copied().unwrap_or_default()
    }
}

/// What one projector's scan collects.
struct Collect {
    decoder: Option<Decoder>,
    patterns: Patterns,
    frames: Option<PathBuf>,
    lumas: Vec<Option<Vec<u8>>>,
    unstable: u32,
    unsettled: Vec<usize>,
    camera_delay_ms: Option<f64>,
}

impl Collect {
    /// Adds the picture of pattern `index`.
    fn add(&mut self, camera: &CameraStream, index: usize, shot: &Shot) -> CmdResult<()> {
        let f = &shot.frame;
        let picture =
            colour::linear_picture(&camera.planes(f), 1).map_err(|e| format!("{}: {e}", camera.name))?;
        let rgba: Vec<[f32; 4]> = picture.rgb.iter().map(|[r, g, b]| [*r, *g, *b, 1.0]).collect();
        let d = match &mut self.decoder {
            Some(d) => d,
            None => {
                self.decoder.insert(Decoder::new(self.patterns, f.width, f.height).map_err(CommandError)?)
            }
        };
        if d.size() != (f.width, f.height) {
            let (w, h) = d.size();
            return Err(format!(
                "camera {} changed its size from {w} x {h} to {} x {}",
                camera.name, f.width, f.height
            )
            .into());
        }
        d.add(index, &rgba).map_err(CommandError)?;
        if let Some(dir) = &self.frames {
            let file = dir.join(format!("{index:02}.png"));
            let png = Pixels { width: f.width, height: f.height, data: rgba }.to_png()?;
            std::fs::write(&file, png).map_err(|e| format!("{}: {e}", file.display()))?;
        }
        if let Some(slot) = self.lumas.get_mut(index) {
            *slot = Some(shot.luma.clone());
        }
        self.unstable += shot.unstable;
        if !shot.settled {
            self.unsettled.push(index);
        }
        if f.delay_ms.is_some() {
            self.camera_delay_ms = f.delay_ms;
        }
        Ok(())
    }
}

/// The projector's size as its outputs draw it: its viewport of its display.
fn projector_size(e: &Engine, net: &Network, id: NodeId) -> (u32, u32) {
    let desc = e.types().desc(PROJECTOR);
    let viewport =
        match net.node(id).zip(desc).and_then(|(n, d)| n.param(d, "viewport")).and_then(Value::components) {
            Some(&[x, y, w, h]) => [x, y, w, h],
            _ => [0.0, 0.0, 1.0, 1.0],
        };
    raster(Some(&e.board().state()), id, viewport)
}

fn seconds(d: Duration) -> f64 {
    (d.as_secs_f64() * 1000.0).round() / 1000.0
}

/// `scan.capture`'s arguments, checked, the defaults filled in.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Settings {
    settle: f64,
    timeout: f64,
    level: f32,
    retries: u32,
    fill: u32,
    keep_frames: bool,
    verify: bool,
}

fn settings(a: &CaptureArgs) -> CmdResult<Settings> {
    let settle = settle_of(a.settle, DEFAULT_SETTLE)?;
    let timeout = timeout_of(a.timeout)?;
    let level = a.level.unwrap_or(1.0);
    if !(level.is_finite() && level > 0.0 && level <= 1.0) {
        return Err("`level` is above 0, at most 1".into());
    }
    let retries = a.retries.unwrap_or(DEFAULT_RETRIES);
    if retries > 20 {
        return Err("`retries` is 0 to 20".into());
    }
    a.options.check().map_err(CommandError)?;
    let fill = fill_of(a.fill)?;
    Ok(Settings {
        settle,
        timeout,
        level,
        retries,
        fill,
        keep_frames: a.frames.unwrap_or(false),
        verify: a.verify.unwrap_or(true),
    })
}

/// Each projector's scan folder name (inside `out`); two may not share one.
fn folders(projectors: &[(NodeId, String)]) -> CmdResult<Vec<String>> {
    let mut folders: Vec<String> = Vec::new();
    for (_, path) in projectors {
        let name = folder_name(path);
        if folders.contains(&name) {
            return Err(format!("two projectors would share the folder {name}").into());
        }
        folders.push(name);
    }
    Ok(folders)
}

/// Every projector shown while scanning: the `scanned` ones and every other `active` one, which
/// shows black the whole time (its picture would light the scene where it overlaps the one
/// scanned).
fn shown_projectors(scanned: &[(NodeId, String)], active: Vec<(NodeId, String)>) -> Vec<(NodeId, String)> {
    let mut shown = scanned.to_vec();
    for p in active {
        if !shown.iter().any(|(id, _)| *id == p.0) {
            shown.push(p);
        }
    }
    shown
}

/// Samples lit by the projector: brighter in `white` than in `black` by more than [`CHANGED`].
fn lit_samples(white: &[u8], black: &[u8]) -> Vec<bool> {
    white.iter().zip(black).map(|(w, b)| i16::from(*w) - i16::from(*b) > CHANGED).collect()
}

/// The pattern-inverse pair ending at `index` is one of the coarsest bits, checked.
fn checked_pair(patterns: &Patterns, index: usize) -> bool {
    match patterns.get(index) {
        Some(Pattern::Columns { bit, inverse: true }) | Some(Pattern::Rows { bit, inverse: true }) => {
            bit < CHECKED_BITS
        }
        _ => false,
    }
}

/// The margin of the pair ending at `index` ([`margin`]), from the luma samples of the pictures
/// taken (`lumas`, by pattern).
fn pair_margin(lumas: &[Option<Vec<u8>>], index: usize) -> Option<f64> {
    let get = |i: usize| lumas.get(i).and_then(|l| l.as_deref());
    match (get(0), get(1), index.checked_sub(1).and_then(get), get(index)) {
        (Some(w), Some(b), Some(p), Some(q)) => margin(w, b, p, q),
        _ => None,
    }
}

/// Per axis (columns, rows) the margin of each bit's pair, rounded to hundredths: for the report.
fn margins(patterns: &Patterns, lumas: &[Option<Vec<u8>>]) -> [Vec<Option<f64>>; 2] {
    let mut margins: [Vec<Option<f64>>; 2] = Default::default();
    for index in 2..patterns.count() {
        let (axis, inverse) = match patterns.get(index) {
            Some(Pattern::Columns { inverse, .. }) => (0, inverse),
            Some(Pattern::Rows { inverse, .. }) => (1, inverse),
            _ => continue,
        };
        if !inverse {
            continue;
        }
        let m = pair_margin(lumas, index);
        if let Some(list) = margins.get_mut(axis) {
            list.push(m.map(|v| (v * 100.0).round() / 100.0));
        }
    }
    margins
}

/// One projector's scan: the pictures decoded so far and what was seen on the way.
struct Scanned {
    collect: Collect,
    /// When the light of white (black) arrived, after it was presented.
    rise: Option<Duration>,
    fall: Option<Duration>,
    /// What each stripe picture waited for the light.
    latency: Duration,
    lit_samples: usize,
    /// Pairs taken again for too small a margin.
    retaken: usize,
    /// Pictures taken without the camera's picture changing.
    unchanged: u32,
    /// From the start of this projector to the last picture.
    captured: Duration,
}

/// Scans projector `k` (`id` at `path`): black, white, black (the latency read from the changes),
/// then every stripe pattern, the coarsest pairs checked and taken again if too alike.
/// `known_latency` is the wait before the first black; `frames`, if given, keeps the pictures.
fn scan_projector(
    s: &mut Scanner<'_>,
    k: usize,
    (id, path): (NodeId, &str),
    patterns: Patterns,
    frames: Option<PathBuf>,
    known_latency: Duration,
) -> CmdResult<Scanned> {
    let t = Instant::now();
    let size = patterns.size();
    if let Some(dir) = &frames {
        // Frames of an earlier scan would mix with these in scan.decode.
        if dir.exists() {
            std::fs::remove_dir_all(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
        }
        std::fs::create_dir_all(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    }
    let mut c = Collect {
        decoder: None,
        patterns,
        frames,
        lumas: vec![None; patterns.count()],
        unstable: 0,
        unsettled: Vec::new(),
        camera_delay_ms: None,
    };

    // Black first, so white is a change the latency can be read from (the latency of the
    // projector before, if any, lets the camera see that black).
    let shown = s.show(id, 1, size)?;
    let black_before = s.take(path, shown, known_latency, None, None, None)?;
    let shown = s.show(id, 0, size)?;
    let white = s.take(
        &format!("{path}, white"),
        shown,
        Duration::ZERO,
        Some((&black_before.luma, Way::Brighter)),
        None,
        None,
    )?;
    c.add(&s.camera, 0, &white)?;
    let shown = s.show(id, 1, size)?;
    let black = s.take(
        &format!("{path}, black"),
        shown,
        Duration::ZERO,
        Some((&white.luma, Way::Darker)),
        None,
        None,
    )?;
    c.add(&s.camera, 1, &black)?;
    let delay = |shot: &Shot| shot.changed_at.map(|t| t.saturating_duration_since(shot.presented));
    let (rise, fall) = (delay(&white), delay(&black));
    // One frame more: the first frame that showed the change may have been exposed early,
    // the next time late.
    let latency = rise.unwrap_or_default().max(fall.unwrap_or_default()) + s.interval();
    let lit = lit_samples(&white.luma, &black.luma);
    let lit_count = lit.iter().filter(|l| **l).count();
    if lit_count == 0 {
        return Err(format!("{path}: white and black look alike to the camera").into());
    }

    // The stripes, pattern and inverse in turn; the coarsest bits checked.
    let mut index = 2;
    let mut retaken: Vec<usize> = Vec::new();
    let mut previous = black.luma.clone();
    let mut unchanged = 0;
    while index < patterns.count() {
        let shown = s.show(id, index, size)?;
        // Never before the latency passed; then after the picture changed from the one before
        // (where the projector reaches), or, for stripes too fine for the camera, once twice
        // the latency more passed. A change alone is no proof: fine stripes flicker in the
        // camera, and a late frame of the pattern before is a change too.
        let shot = s.take(path, shown, latency, Some((&previous, Way::Any)), Some(&lit), Some(latency))?;
        c.add(&s.camera, index, &shot)?;
        if let Some(w) = s.watch {
            (w.progress)(k, index as f32 / patterns.count().max(1) as f32);
        }
        if shot.changed_at.is_none() {
            unchanged += 1;
        }
        previous = shot.luma;
        if checked_pair(&patterns, index) && !retaken.contains(&index) {
            let m = pair_margin(&c.lumas, index);
            if m.is_some_and(|m| m < MIN_MARGIN) {
                // Once more from the pattern: adding it again restarts the pair.
                retaken.push(index);
                index -= 1;
                continue;
            }
        }
        index += 1;
    }
    Ok(Scanned {
        collect: c,
        rise,
        fall,
        latency,
        lit_samples: lit_count,
        retaken: retaken.len(),
        unchanged,
        captured: t.elapsed(),
    })
}

/// What one projector's scan warns of: pairs that barely differ, pictures that kept changing,
/// a latency that could not be read.
fn scan_warnings(
    path: &str,
    margins: &[Vec<Option<f64>>; 2],
    unsettled: &[usize],
    retries: u32,
    rise: Option<Duration>,
    fall: Option<Duration>,
) -> Vec<String> {
    let mut warnings = Vec::new();
    for (axis, name) in ["column", "row"].iter().enumerate() {
        for (bit, m) in margins.get(axis).into_iter().flatten().enumerate().take(CHECKED_BITS as usize) {
            if m.is_some_and(|m| m < MIN_MARGIN) {
                warnings.push(format!(
                    "{path}: {name} bit {bit}'s pattern and inverse barely differ in the camera (median margin {:.2}): \
                     stray light, a moving camera, or the camera adapting its exposure?",
                    m.unwrap_or(0.0)
                ));
            }
        }
    }
    if !unsettled.is_empty() {
        warnings.push(format!(
            "{path}: the camera's picture of pattern(s) {unsettled:?} still changed after {retries} retries; taken anyway"
        ));
    }
    if rise.is_none() || fall.is_none() {
        warnings.push(format!("{path}: the light-to-camera latency could not be read"));
    }
    warnings
}

/// One projector's part of the reply.
struct ProjectorScan {
    path: String,
    rise: Option<Duration>,
    fall: Option<Duration>,
    valid: usize,
    measured: usize,
    report: Json,
}

/// The reply's entry of a projector scanned (`scanned`) and decoded (`decoded`) into `folder`.
fn projector_report(
    path: &str,
    folder: &std::path::Path,
    scanned: &Scanned,
    decoded: &Decoded,
    margins: &[Vec<Option<f64>>; 2],
    warnings: &[String],
) -> Json {
    let c = &scanned.collect;
    let pictures = c.patterns.count();
    let mut o = decoded.fields.clone();
    o.insert("path".into(), json!(path));
    o.insert("folder".into(), json!(folder.display().to_string()));
    o.insert(
        "latency_ms".into(),
        json!({
            "rise": scanned.rise.map(ms),
            "fall": scanned.fall.map(ms),
            "waited": ms(scanned.latency),
            "camera_delay": c.camera_delay_ms,
        }),
    );
    o.insert("margins".into(), json!({"columns": margins[0], "rows": margins[1]}));
    o.insert("lit_samples".into(), json!(scanned.lit_samples));
    o.insert("retaken_pairs".into(), json!(scanned.retaken));
    o.insert("unstable_frames".into(), json!(c.unstable));
    o.insert("unchanged_pictures".into(), json!(scanned.unchanged));
    o.insert("frames".into(), json!(c.frames.as_ref().map(|f| f.display().to_string())));
    o.insert(
        "ms".into(),
        json!({
            "capture": ms(scanned.captured),
            "decode": ms(decoded.decode),
            "write": ms(decoded.write),
            "per_picture": ms(scanned.captured / u32::try_from(pictures + 1).unwrap_or(1)),
        }),
    );
    o.insert("warnings".into(), json!(warnings));
    Json::Object(o)
}

/// The largest of `values`, `None` without any.
fn largest(values: impl Iterator<Item = f64>) -> Option<f64> {
    values.fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v))))
}

/// The reply of the whole capture.
fn capture_reply(
    a: &CaptureArgs,
    folders: &[String],
    scans: Vec<ProjectorScan>,
    warnings: Vec<String>,
    started: Instant,
) -> Json {
    let summary: Vec<String> = scans
        .iter()
        .map(|r| format!("{}: {} camera px decoded, {} projector px measured", r.path, r.valid, r.measured))
        .collect();
    let rise = largest(scans.iter().filter_map(|r| r.rise.map(ms)));
    let fall = largest(scans.iter().filter_map(|r| r.fall.map(ms)));
    let folders_json: Vec<String> = folders
        .iter()
        .map(|f| {
            a.out
                .as_deref()
                .unwrap_or(std::path::Path::new(DEFAULT_OUT))
                .join(f)
                .display()
                .to_string()
                .replace('\\', "/")
        })
        .collect();
    let count = scans.len();
    let projectors: Vec<Json> = scans.into_iter().map(|r| r.report).collect();
    json!({
        "message": format!("{} projector(s) scanned in {:.1} s; {}", count, started.elapsed().as_secs_f64(), summary.join("; ")),
        "projectors": projectors,
        "latency_ms": {"rise": rise, "fall": fall},
        "scans": folders_json,
        "warnings": warnings,
        "seconds": seconds(started.elapsed()),
        "next": "scan.align with these scans and the mappers of the projectors (same order); scan.objects / scan.blend take the folders too",
    })
}

pub fn capture(e: &Engine, a: &CaptureArgs) -> CmdResult<Json> {
    capture_watched(e, a, None)
}

/// [`capture`], stopped by and reporting to `watch`. In stages: the settings; the projectors,
/// their folders and patterns, the camera and the claim on them; per projector the scan
/// ([`scan_projector`]), then its decode written and its report; the reply ([`capture_reply`]).
pub fn capture_watched(e: &Engine, a: &CaptureArgs, watch: Option<&Watch<'_>>) -> CmdResult<Json> {
    let started = Instant::now();
    let Settings { settle, timeout, level, retries, fill, keep_frames, verify } = settings(a)?;

    let session = e.session();
    let net = session.doc().snapshot();
    let projectors = projector_list(e, &net, a.projectors.as_deref())?;
    if projectors.is_empty() {
        return Err(format!("no active {PROJECTOR} to scan").into());
    }
    let folders = folders(&projectors)?;
    let sizes: Vec<(u32, u32)> = projectors.iter().map(|(id, _)| projector_size(e, &net, *id)).collect();
    let mut all_patterns = Vec::new();
    for (size, (_, path)) in sizes.iter().zip(&projectors) {
        all_patterns.push(Patterns::new(size.0, size.1).map_err(|err| format!("{path}: {err}"))?);
    }
    let camera = CameraStream::open(e, &net, &a.camera)?;
    let out = session.dir().join(a.out.as_deref().unwrap_or(std::path::Path::new(DEFAULT_OUT)));

    let shown = shown_projectors(&projectors, projector_list(e, &net, None)?);
    let claim = Claim::new(e, shown.iter().map(|(id, _)| *id).collect())?;
    let mut s = Scanner {
        e,
        projectors: shown,
        camera,
        settle: Duration::from_secs_f64(settle),
        timeout: Duration::from_secs_f64(timeout),
        verify,
        retries,
        level,
        pass: e.output().pass,
        intervals: Vec::new(),
        watch,
    };

    let mut collected = Vec::new();
    let mut all_warnings: Vec<String> = Vec::new();
    // Before the first projector the latency is unknown and the content shown before may still be
    // in the camera's picture: a generous wait once.
    let mut known_latency = FIRST_WAIT;
    for (k, (id, path)) in projectors.iter().enumerate() {
        let patterns = *all_patterns.get(k).ok_or("lost a projector")?;
        let folder = out.join(folders.get(k).map(String::as_str).unwrap_or("projector"));
        let frames = keep_frames.then(|| folder.join(FRAMES));
        let mut scanned = scan_projector(&mut s, k, (*id, path), patterns, frames, known_latency)?;
        known_latency = scanned.latency;

        let margins = margins(&patterns, &scanned.collect.lumas);
        let mut warnings =
            scan_warnings(path, &margins, &scanned.collect.unsettled, retries, scanned.rise, scanned.fall);
        let decoder = scanned.collect.decoder.take().ok_or("no camera pictures")?;
        let decoded = write_decoded(&decoder, &a.options, fill, &folder)?;
        warnings.extend(decoded.warnings.iter().map(|w| format!("{path}: {w}")));
        let report = projector_report(path, &folder, &scanned, &decoded, &margins, &warnings);
        all_warnings.extend(warnings);
        collected.push(ProjectorScan {
            path: path.clone(),
            rise: scanned.rise,
            fall: scanned.fall,
            valid: decoded.valid,
            measured: decoded.measured,
            report,
        });
    }
    drop(claim);
    Ok(capture_reply(a, &folders, collected, all_warnings, started))
}

#[cfg(test)]
mod tests {
    use super::*;
    use penumbra_camera::{Matrix, PixelFormat};

    fn frame(width: u32, height: u32, y: Vec<u8>) -> CameraFrame {
        CameraFrame {
            width,
            height,
            format: PixelFormat::I420,
            cb: vec![128; y.len()],
            cr: vec![128; y.len()],
            y,
            shift: (0, 0),
            matrix: Matrix::Bt709,
            full_range: true,
            sequence: 0,
            arrived: Instant::now(),
            delay_ms: None,
        }
    }

    fn two() -> Vec<(NodeId, String)> {
        vec![(NodeId(1), "/a".into()), (NodeId(2), "/b".into())]
    }

    #[test]
    fn luma_samples_every_fourth_pixel() {
        // 9 x 5: columns 0, 4, 8 of rows 0 and 4.
        let y: Vec<u8> = (0..45).collect();
        assert_eq!(luma(&frame(9, 5, y)), [0, 4, 8, 36, 40, 44]);
        // Too few bytes read as 0.
        assert_eq!(luma(&frame(5, 1, vec![7])), [7, 0]);
    }

    #[test]
    fn changed_counts_by_way_and_mask() {
        let a = [100, 100, 100, 100];
        let b = [100, 113, 87, 112];
        assert_eq!(changed(&a, &b, None, Way::Any), (2, 4));
        assert_eq!(changed(&a, &b, None, Way::Brighter), (1, 4));
        assert_eq!(changed(&a, &b, None, Way::Darker), (1, 4));
        let mask = [true, true, false];
        assert_eq!(changed(&a, &b, Some(&mask), Way::Any), (1, 2));
    }

    #[test]
    fn margin_is_the_median_where_the_projector_reaches() {
        let white = [200, 200, 200, 20];
        let black = [0, 0, 0, 10];
        let pattern = [200, 100, 0, 20];
        let inverse = [0, 0, 160, 10];
        // 1, 0.5, 0.8; the last sample is not reached.
        assert_eq!(margin(&white, &black, &pattern, &inverse), Some(0.8));
        assert_eq!(margin(&[10], &[0], &[10], &[0]), None);
    }

    #[test]
    fn lit_samples_are_clearly_brighter_in_white() {
        assert_eq!(lit_samples(&[100, 113, 112, 0], &[100, 100, 100, 50]), [false, true, false, false]);
    }

    #[test]
    fn pairs_margins_and_the_checked_bits() {
        let patterns = Patterns::new(8, 4).unwrap();
        let (cb, rb) = (patterns.column_bits() as usize, patterns.row_bits() as usize);
        // The inverse of each pair is checked for the coarsest bits only.
        assert!(!checked_pair(&patterns, 2) && checked_pair(&patterns, 3) && checked_pair(&patterns, 5));
        assert_eq!(cb, 3);
        assert!(!checked_pair(&patterns, 2 + 2 * CHECKED_BITS as usize + 1), "column bit 2");
        assert!(checked_pair(&patterns, 2 + 2 * cb + 1));
        assert!(!checked_pair(&patterns, 0) && !checked_pair(&patterns, patterns.count()));

        // White, black, then every pair half as far apart as the one before.
        let mut lumas: Vec<Option<Vec<u8>>> = vec![Some(vec![200; 4]), Some(vec![0; 4])];
        let mut level = 200u8;
        while lumas.len() < patterns.count() {
            lumas.push(Some(vec![level; 4]));
            lumas.push(Some(vec![0; 4]));
            level /= 2;
        }
        assert_eq!(pair_margin(&lumas, 3), Some(1.0));
        assert_eq!(pair_margin(&lumas, 5), Some(0.5));
        assert_eq!(pair_margin(&lumas, 0), None);
        let [columns, rows] = margins(&patterns, &lumas);
        assert_eq!((columns.len(), rows.len()), (cb, rb));
        assert_eq!(columns[..2], [Some(1.0), Some(0.5)]);
        assert_eq!(rows[0], Some(0.13), "{rows:?}");
        // A picture missing: no margin for its pair.
        lumas[4] = None;
        assert_eq!(margins(&patterns, &lumas)[0][1], None);
    }

    #[test]
    fn warnings_of_a_scan() {
        let margins = [vec![Some(0.9), Some(0.2), Some(0.1)], vec![None, Some(0.3)]];
        let ms = Some(Duration::from_millis(80));
        let w = scan_warnings("/a", &margins, &[], 3, ms, ms);
        // Only the checked bits: column 1 and row 1; not column 2.
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w[0].starts_with("/a: column bit 1's") && w[0].contains("margin 0.20"));
        assert!(w[1].starts_with("/a: row bit 1's"));
        let w = scan_warnings("/a", &Default::default(), &[4, 7], 3, None, ms);
        assert_eq!(
            w,
            [
                "/a: the camera's picture of pattern(s) [4, 7] still changed after 3 retries; taken anyway",
                "/a: the light-to-camera latency could not be read",
            ]
        );
    }

    #[test]
    fn folders_and_the_projectors_shown() {
        assert_eq!(folders(&two()).unwrap(), [folder_name("/a"), folder_name("/b")]);
        let same = vec![(NodeId(1), "/a".to_string()), (NodeId(2), "/a".to_string())];
        assert!(folders(&same).unwrap_err().0.contains("share the folder"));
        let active = vec![(NodeId(3), "/c".to_string()), (NodeId(1), "/a".to_string())];
        let shown = shown_projectors(&two(), active);
        assert_eq!(shown.iter().map(|(id, _)| id.0).collect::<Vec<_>>(), [1, 2, 3]);
    }

    #[test]
    fn settings_fill_defaults_and_check_ranges() {
        let args = |v: Json| -> CaptureArgs { serde_json::from_value(v).unwrap() };
        let s = settings(&args(json!({"camera": "/cam"}))).unwrap();
        assert_eq!(
            (s.settle, s.level, s.retries, s.keep_frames, s.verify),
            (DEFAULT_SETTLE, 1.0, 3, false, true)
        );
        for (bad, says) in [
            (json!({"level": 0.0}), "`level`"),
            (json!({"level": 1.5}), "`level`"),
            (json!({"retries": 21}), "`retries`"),
            (json!({"timeout": 0.0}), "`timeout`"),
            (json!({"fill": 100000}), "`fill`"),
        ] {
            let mut v = bad.clone();
            v["camera"] = json!("/cam");
            let err = settings(&args(v)).unwrap_err();
            assert!(err.0.contains(says), "{bad}: {}", err.0);
        }
    }

    #[test]
    fn the_reply_sums_up_the_projectors() {
        let a: CaptureArgs = serde_json::from_value(json!({"camera": "/cam", "out": "scans"})).unwrap();
        let scan = |path: &str, rise: Option<u64>, valid| ProjectorScan {
            path: path.into(),
            rise: rise.map(Duration::from_millis),
            fall: Some(Duration::from_millis(40)),
            valid,
            measured: 10,
            report: json!({"path": path}),
        };
        let scans = vec![scan("/a", Some(70), 100), scan("/b", None, 200)];
        let r = capture_reply(&a, &["a".into(), "b".into()], scans, vec!["w".into()], Instant::now());
        assert_eq!(r["latency_ms"], json!({"rise": 70.0, "fall": 40.0}));
        assert_eq!(r["scans"], json!(["scans/a", "scans/b"]));
        assert_eq!(r["projectors"], json!([{"path": "/a"}, {"path": "/b"}]));
        let message = r["message"].as_str().unwrap();
        assert!(message.starts_with("2 projector(s) scanned in "), "{message}");
        assert!(
            message.ends_with(
                "/a: 100 camera px decoded, 10 projector px measured; /b: 200 camera px decoded, 10 projector px measured"
            ),
            "{message}"
        );
        assert_eq!(largest([].into_iter()), None);
        assert_eq!(seconds(Duration::from_micros(1_234_567)), 1.235);
    }
}
