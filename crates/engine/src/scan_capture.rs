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
use crate::scan::{fill_of, ms, write_decoded};

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
        "Scan projectors with a camera, live: each alone shows the structured light patterns (in its own \
         pixels, past its colour correction, the network untouched) while the others show black; the \
         camera's frames are decoded and written as scan.decode writes them, into out/<projector> (the \
         path without its leading slash, slashes as _); measures the light-to-camera latency on the way; \
         blocks about 15 s per projector; {camera (tex.video_in path), projectors? (paths, default every \
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

    fn luma(&self, index: usize) -> Option<&[u8]> {
        self.lumas.get(index).and_then(|l| l.as_deref())
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

pub fn capture(e: &Engine, a: &CaptureArgs) -> CmdResult<Json> {
    capture_watched(e, a, None)
}

/// [`capture`], stopped by and reporting to `watch`.
pub fn capture_watched(e: &Engine, a: &CaptureArgs, watch: Option<&Watch<'_>>) -> CmdResult<Json> {
    let started = Instant::now();
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
    let keep_frames = a.frames.unwrap_or(false);

    let net = e.session().doc().snapshot();
    let projectors = projector_list(e, &net, a.projectors.as_deref())?;
    if projectors.is_empty() {
        return Err(format!("no active {PROJECTOR} to scan").into());
    }
    let mut folders: Vec<String> = Vec::new();
    for (_, path) in &projectors {
        let name = folder_name(path);
        if folders.contains(&name) {
            return Err(format!("two projectors would share the folder {name}").into());
        }
        folders.push(name);
    }
    let sizes: Vec<(u32, u32)> = projectors.iter().map(|(id, _)| projector_size(e, &net, *id)).collect();
    let mut all_patterns = Vec::new();
    for (size, (_, path)) in sizes.iter().zip(&projectors) {
        all_patterns.push(Patterns::new(size.0, size.1).map_err(|err| format!("{path}: {err}"))?);
    }
    let camera = CameraStream::open(e, &net, &a.camera)?;
    let out = e.session().dir().join(a.out.as_deref().unwrap_or(std::path::Path::new(DEFAULT_OUT)));

    // Every active projector not scanned shows black the whole time: its picture would light the
    // scene where it overlaps the one scanned.
    let mut shown = projectors.clone();
    for p in projector_list(e, &net, None)? {
        if !shown.iter().any(|(id, _)| *id == p.0) {
            shown.push(p);
        }
    }
    let claim = Claim::new(e, shown.iter().map(|(id, _)| *id).collect())?;
    let mut s = Scanner {
        e,
        projectors: shown,
        camera,
        settle: Duration::from_secs_f64(settle),
        timeout: Duration::from_secs_f64(timeout),
        verify: a.verify.unwrap_or(true),
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
        let id = *id;
        let t = Instant::now();
        let patterns = *all_patterns.get(k).ok_or("lost a projector")?;
        let size = patterns.size();
        let folder = out.join(folders.get(k).map(String::as_str).unwrap_or("projector"));
        let frames = keep_frames.then(|| folder.join(FRAMES));
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
        let mut warnings: Vec<String> = Vec::new();

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
        known_latency = latency;
        let lit: Vec<bool> = white
            .luma
            .iter()
            .zip(&black.luma)
            .map(|(w, b)| i16::from(*w) - i16::from(*b) > CHANGED)
            .collect();
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
            let shot =
                s.take(path, shown, latency, Some((&previous, Way::Any)), Some(&lit), Some(latency))?;
            c.add(&s.camera, index, &shot)?;
            if let Some(w) = watch {
                (w.progress)(k, index as f32 / patterns.count().max(1) as f32);
            }
            if shot.changed_at.is_none() {
                unchanged += 1;
            }
            previous = shot.luma;
            let checked = match patterns.get(index) {
                Some(Pattern::Columns { bit, inverse: true })
                | Some(Pattern::Rows { bit, inverse: true }) => bit < CHECKED_BITS,
                _ => false,
            };
            if checked && !retaken.contains(&index) {
                let m = match (c.luma(0), c.luma(1), c.luma(index - 1), c.luma(index)) {
                    (Some(w), Some(b), Some(p), Some(q)) => margin(w, b, p, q),
                    _ => None,
                };
                if m.is_some_and(|m| m < MIN_MARGIN) {
                    // Once more from the pattern: adding it again restarts the pair.
                    retaken.push(index);
                    index -= 1;
                    continue;
                }
            }
            index += 1;
        }
        let captured = t.elapsed();

        // Margins per bit, for the report.
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
            let m = match (c.luma(0), c.luma(1), c.luma(index - 1), c.luma(index)) {
                (Some(w), Some(b), Some(p), Some(q)) => margin(w, b, p, q),
                _ => None,
            };
            if let Some(list) = margins.get_mut(axis) {
                list.push(m.map(|v| (v * 100.0).round() / 100.0));
            }
        }
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
        if !c.unsettled.is_empty() {
            warnings.push(format!(
                "{path}: the camera's picture of pattern(s) {:?} still changed after {retries} retries; taken anyway",
                c.unsettled
            ));
        }
        if rise.is_none() || fall.is_none() {
            warnings.push(format!("{path}: the light-to-camera latency could not be read"));
        }

        let decoder = c.decoder.take().ok_or("no camera pictures")?;
        let mut result = write_decoded(&decoder, &a.options, fill, &folder)?;
        if let Some(w) = result.get("warnings").and_then(Json::as_array) {
            warnings.extend(w.iter().filter_map(Json::as_str).map(|w| format!("{path}: {w}")));
        }
        let decode_ms = result.get("ms").cloned().unwrap_or(Json::Null);
        if let Some(o) = result.as_object_mut() {
            for key in ["warnings", "ms", "next", "message"] {
                o.remove(key);
            }
            o.insert("path".into(), json!(path));
            o.insert("folder".into(), json!(folder.display().to_string()));
            o.insert(
                "latency_ms".into(),
                json!({
                    "rise": rise.map(ms),
                    "fall": fall.map(ms),
                    "waited": ms(latency),
                    "camera_delay": c.camera_delay_ms,
                }),
            );
            o.insert("margins".into(), json!({"columns": margins[0], "rows": margins[1]}));
            o.insert("lit_samples".into(), json!(lit_count));
            o.insert("retaken_pairs".into(), json!(retaken.len()));
            o.insert("unstable_frames".into(), json!(c.unstable));
            o.insert("unchanged_pictures".into(), json!(unchanged));
            o.insert("frames".into(), json!(c.frames.as_ref().map(|f| f.display().to_string())));
            o.insert(
                "ms".into(),
                json!({
                    "capture": ms(captured),
                    "decode": decode_ms.get("decode"),
                    "write": decode_ms.get("write"),
                    "per_picture": ms(captured / u32::try_from(patterns.count() + 1).unwrap_or(1)),
                }),
            );
            o.insert("warnings".into(), json!(warnings));
        }
        all_warnings.extend(warnings);
        collected.push(result);
    }
    drop(claim);

    let rises: Vec<f64> =
        collected.iter().filter_map(|r| r.pointer("/latency_ms/rise").and_then(Json::as_f64)).collect();
    let falls: Vec<f64> =
        collected.iter().filter_map(|r| r.pointer("/latency_ms/fall").and_then(Json::as_f64)).collect();
    let summary: Vec<String> = collected
        .iter()
        .map(|r| {
            format!(
                "{}: {} camera px decoded, {} projector px measured",
                r["path"].as_str().unwrap_or("?"),
                r.pointer("/camera_pixels/valid").unwrap_or(&Json::Null),
                r.pointer("/projector_pixels/measured").unwrap_or(&Json::Null)
            )
        })
        .collect();
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
    Ok(json!({
        "message": format!("{} projector(s) scanned in {:.1} s; {}", collected.len(), started.elapsed().as_secs_f64(), summary.join("; ")),
        "projectors": collected,
        "latency_ms": {
            "rise": rises.iter().copied().fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v)))),
            "fall": falls.iter().copied().fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v)))),
        },
        "scans": folders_json,
        "warnings": all_warnings,
        "seconds": seconds(started.elapsed()),
        "next": "scan.align with these scans and the mappers of the projectors (same order); scan.objects / scan.blend take the folders too",
    }))
}
