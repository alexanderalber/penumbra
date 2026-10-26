//! `tex.video_file`: a video file or animated GIF, decoded on threads of its own
//! (`penumbra_video::Player`), its planes (or a HAP frame's compressed blocks) converted to a
//! texture on the GPU. Loops are
//! seamless: the start of the next pass is decoded before the end of this one is shown.
//!
//! The playhead follows the show clock while the node cooks, times `speed`. A node that stops
//! cooking (nothing downstream wants it) pauses there and resumes where it was, instead of
//! seeking to wherever the show time has moved meanwhile.
//!
//! A restart starts the clip over from `start`: a rising edge on the `restart` param (a bound
//! channel crossing 0.5), a request through [`VideoRestarts`] (`video_file.restart`, the panel's
//! button), or, with `restart_on_show`, the node coming back into use after half a second or more
//! out of it (a switch or sequence picked it again; thumbnails do not count). The player keeps
//! the frames at `start` decoded where it can, so the restart shows at once.
//!
//! The `info` output carries channels about playback, updated with every new frame: `frame`,
//! `progress` (0 to 1 through the clip), `loops` (passes completed since the last restart),
//! `late` (frames that were due before they were decoded), `seeks` and `restarts`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use penumbra_core::{Channel, Chans, Error, Name, NodeId, ParamSpec, Unit, lock};
use penumbra_gpu::bc::{BcColor, BcConverter, BcFormat, BcFrame};
use penumbra_gpu::video::{Converter, Planes, Transfer, VideoColor, VideoPlanes};
use penumbra_gpu::{Gpu, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FileWatch, FnNodeType, JobStatus, NodeDesc, NodeTypes,
    Operator, Waker,
};
use penumbra_video::{
    Frame, HapColor, HapFrame, Limits, Matrix, PixelData, Player, Range, Shown, VideoFrame,
};

use crate::on_gpu;

/// The node kind.
pub const VIDEO_FILE: &str = "tex.video_file";

/// A pause in cooking longer than this is not played through (see the module docs).
const GAP_SECONDS: f64 = 0.5;
/// How long a late frame keeps the node's warning up, in show seconds.
const LATE_WARNING_SECONDS: f64 = 2.0;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(
        FnNodeType::new(
            NodeDesc::new(VIDEO_FILE, "Plays a video file or animated GIF, looping without a gap")
                .param(
                    ParamSpec::file("file", penumbra_video::EXTENSIONS).doc(
                        "MP4, MOV, MKV, WebM, MXF, MPEG (H.264, HEVC, ProRes, DNxHR, VP9, AV1, HAP) or GIF",
                    ),
                )
                .param(ParamSpec::bool("play", true).doc("Off holds the current frame"))
                .param(
                    ParamSpec::float("speed", 1.0)
                        .clamp(0.0, 16.0)
                        .range(0.0, 4.0)
                        .doc("Playback rate; every frame is still decoded, so high speeds cost CPU"),
                )
                .param(ParamSpec::bool("loop", true).doc("Off stops on the last frame"))
                .param(
                    ParamSpec::float("start", 0.0)
                        .clamp(0.0, 1e7)
                        .range(0.0, 60.0)
                        .unit(Unit::Seconds)
                        .more()
                        .doc("Where playback begins when the file is loaded and where a restart goes"),
                )
                .param(
                    ParamSpec::bool("restart", false)
                        .doc("Turning on starts the clip over from `start`; bind a channel to cue it"),
                )
                .param(ParamSpec::bool("restart_on_show", false).doc(
                    "Starts over whenever the clip comes back into use, such as a switch or sequence picking it",
                ))
                .output("out", DataKind::Tex)
                .output("info", DataKind::Chan)
                .describe_output("out", "The video's picture, at the file's size")
                .describe_output("info", "Playback: frame, progress (0 to 1), loops, late, seeks, restarts")
                .time_dependent(),
            || Box::new(Video::default()) as Box<dyn Operator>,
        )
        .warm_up(on_gpu(|gpu| gpu.video_pipeline().map(|_| ()))),
    )
}

#[derive(Default)]
struct Video {
    player: Option<Player>,
    watch: Option<FileWatch>,
    converter: Converter,
    bc: BcConverter,
    ring: TexRing,
    /// Seconds into the clip (not wrapped).
    playhead: f64,
    /// Show time of the last cook.
    last_time: Option<f64>,
    /// Late frames counted so far, and the show time the count last grew.
    late: (u64, f64),
    has_output: bool,
    /// The file could not be opened.
    failed: Option<String>,
    /// `restart` at the last cook (`None` before the first), for its rising edge.
    restart_was: Option<bool>,
    /// Restart requests through [`VideoRestarts`] taken so far (`None` before the first cook).
    requests: Option<u64>,
}

/// Restart requests from outside the cook (`video_file.restart`, the panel's button), by node.
/// Provided by the engine as a cooker resource; cheap to clone.
#[derive(Clone, Default)]
pub struct VideoRestarts(Arc<Mutex<BTreeMap<NodeId, Entry>>>);

struct Entry {
    waker: Waker,
    requested: u64,
}

impl VideoRestarts {
    pub fn new() -> Self {
        Self::default()
    }

    /// The node starts its clip over at its next cook. False when it has not cooked yet.
    pub fn request(&self, node: NodeId) -> bool {
        let mut entries = lock(&self.0);
        let Some(e) = entries.get_mut(&node) else { return false };
        e.requested += 1;
        e.waker.wake();
        true
    }

    /// Forgets nodes not in `keep`.
    pub fn prune(&self, keep: impl Fn(NodeId) -> bool) {
        lock(&self.0).retain(|id, _| keep(*id));
    }

    /// Registers the node's waker; the number of requests made so far.
    fn check_in(&self, node: NodeId, waker: Waker) -> u64 {
        let mut entries = lock(&self.0);
        let e = entries.entry(node).or_insert(Entry { waker: waker.clone(), requested: 0 });
        e.waker = waker;
        e.requested
    }
}

impl Video {
    /// Whether this cook starts the clip over (see the module docs). Reads every trigger, so
    /// edges and requests are taken even while the file is not open yet.
    fn restart_due(&mut self, cx: &CookContext<'_>) -> Result<bool, CookError> {
        let on = cx.bool("restart")?;
        let edge = on && self.restart_was == Some(false);
        self.restart_was = Some(on);
        let requested = cx.resource::<VideoRestarts>().map_or(0, |r| r.check_in(cx.node(), cx.waker()));
        let asked = self.requests.is_some_and(|r| requested > r);
        self.requests = Some(requested);
        let back = cx.bool("restart_on_show")? && cx.since_last_use().is_some_and(|gap| gap > GAP_SECONDS);
        Ok(edge || asked || back)
    }
}

impl Operator for Video {
    fn trim(&mut self) {
        self.ring.trim();
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = super::gpu(cx)?;
        let path = cx.resolve_path(cx.text("file")?)?;
        let moved = self.watch.as_ref().is_none_or(|w| w.path() != path);
        let changed = self.watch.as_ref().is_some_and(FileWatch::take_changed);
        if moved || changed || self.player.is_none() {
            if moved {
                self.watch = Some(cx.watch_file(&path));
            }
            let limits = Limits { max_side: gpu.max_texture_size(), ..Limits::default() };
            let waker = cx.waker();
            let on_frame: penumbra_video::OnFrame = std::sync::Arc::new(move || waker.wake());
            self.player = Some(Player::new(&path, limits, Some(on_frame)).map_err(CookError::failed)?);
            self.playhead = cx.float("start")?;
            self.failed = None;
            self.late = (0, 0.0);
        }
        let start = cx.float("start")?;
        let restart = self.restart_due(cx)?;
        let now = cx.time().seconds();
        let dt = match self.last_time {
            Some(last) if (0.0..=GAP_SECONDS).contains(&(now - last)) => now - last,
            // A gap or a clock that went back: one show frame.
            Some(_) => f64::from(cx.rate().den()) / f64::from(cx.rate().num().max(1)),
            None => 0.0,
        };
        self.last_time = Some(now);
        if restart {
            self.playhead = start;
        } else if cx.bool("play")? {
            self.playhead += dt * cx.float("speed")?.clamp(0.0, 16.0);
        }
        let looping = cx.bool("loop")?;
        let Some(player) = self.player.as_mut() else {
            return Err(CookError::Pending);
        };
        player.set_cue(start);
        if restart {
            player.restart();
        }
        let shown = player.show(self.playhead, looping);
        let stats = player.stats();
        if stats.late > self.late.0 {
            self.late = (stats.late, now);
        }
        if stats.late > 0 && now - self.late.1 < LATE_WARNING_SECONDS {
            cx.warn(format!("decoding falls behind: {} late frames so far", stats.late));
        }
        match shown {
            Err(e) if self.has_output => {
                cx.warn(e);
                Ok(Cooked::Unchanged)
            }
            Err(e) => {
                self.failed = Some(e.clone());
                Err(CookError::failed(e))
            }
            Ok(Shown::New(frame)) => {
                let target = self.ring.next(gpu, frame.width(), frame.height(), VIDEO_FILE)?;
                match &frame {
                    Frame::Pixels(f) => convert(gpu, &mut self.converter, f, &target)?,
                    Frame::Hap(f) => convert_hap(gpu, &mut self.bc, f, &target)?,
                }
                self.has_output = true;
                let info = info(player, cx.rate().as_f64())?;
                Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, target), Data::new(DataKind::Chan, info)]))
            }
            Ok(Shown::Same | Shown::Late) => Ok(Cooked::Unchanged),
            Ok(Shown::Waiting) if self.has_output => Ok(Cooked::Unchanged),
            Ok(Shown::Waiting) => Err(CookError::Pending),
        }
    }

    /// Running while the file opens and the first frame decodes (so `node.snapshot` waits for
    /// it), idle afterwards: playback itself is not a job.
    fn job_status(&self) -> Option<JobStatus> {
        Some(match (&self.failed, &self.player, self.has_output) {
            (Some(e), _, _) => JobStatus::Failed { error: e.clone() },
            (None, Some(_), false) => JobStatus::Running { progress: None },
            _ => JobStatus::Idle,
        })
    }
}

/// The `info` channels.
fn info(player: &mut Player, rate: f64) -> Result<Chans, CookError> {
    let (pass, frame) = player.position().unwrap_or((0, 0));
    let stats = player.stats();
    let frames = player.info().ok().flatten().map_or(1, |i| i.frames.max(1));
    let progress = if frames > 1 { frame as f64 / (frames - 1) as f64 } else { 0.0 };
    let values = [
        ("frame", frame as f64),
        ("progress", progress),
        ("loops", pass as f64),
        ("late", stats.late as f64),
        ("seeks", stats.seeks as f64),
        ("restarts", stats.restarts as f64),
    ];
    let mut channels = Vec::with_capacity(values.len());
    for (name, v) in values {
        channels.push(Channel { name: Name::new(name)?, samples: vec![v] });
    }
    Ok(Chans::new(rate, channels)?)
}

/// Uploads a decoded frame and converts it into `target`.
fn convert(
    gpu: &Gpu,
    converter: &mut Converter,
    frame: &VideoFrame,
    target: &penumbra_gpu::Tex,
) -> Result<(), CookError> {
    let c = frame.color;
    let color = VideoColor {
        kr_kb: match c.matrix {
            Matrix::Bt601 => (0.299, 0.114),
            Matrix::Bt709 => (0.2126, 0.0722),
            Matrix::Bt2020Ncl => (0.2627, 0.0593),
        },
        full_range: c.range == Range::Full,
        transfer: match c.transfer {
            penumbra_video::Transfer::Linear => Transfer::Linear,
            penumbra_video::Transfer::Pq => Transfer::Pq,
            penumbra_video::Transfer::Hlg => Transfer::Hlg,
            penumbra_video::Transfer::Bt709 | penumbra_video::Transfer::Srgb => Transfer::Srgb,
        },
    };
    let (planes, shift) = match &frame.data {
        PixelData::Yuv8 { planes: [y, cb, cr], chroma, alpha } => (
            Planes::Yuv8 {
                planes: [y.as_slice(), cb.as_slice(), cr.as_slice()],
                alpha: alpha.as_deref().map(Vec::as_slice),
            },
            chroma.shifts(),
        ),
        PixelData::Yuv16 { planes: [y, cb, cr], chroma, bits, alpha } => (
            Planes::Yuv16 {
                planes: [y.as_slice(), cb.as_slice(), cr.as_slice()],
                alpha: alpha.as_deref().map(Vec::as_slice),
                bits: *bits,
            },
            chroma.shifts(),
        ),
        PixelData::Rgba8(data) => (Planes::Rgba8(data.as_slice()), (0, 0)),
        PixelData::RgbaF32(_) => return Err(CookError::failed("float video frames are not supported")),
    };
    let planes = VideoPlanes { width: frame.width, height: frame.height, shift, planes, color };
    converter.convert(gpu, &planes, target).map_err(CookError::from)
}

/// Uploads a HAP frame's blocks and converts them into `target`.
fn convert_hap(
    gpu: &Gpu,
    converter: &mut BcConverter,
    frame: &HapFrame,
    target: &penumbra_gpu::Tex,
) -> Result<(), CookError> {
    let format = match frame.texture.format {
        penumbra_video::BcFormat::Bc1 => BcFormat::Bc1,
        penumbra_video::BcFormat::Bc3 => BcFormat::Bc3,
        penumbra_video::BcFormat::Bc4 => BcFormat::Bc4,
        penumbra_video::BcFormat::Bc7 => BcFormat::Bc7,
    };
    let color = match frame.color {
        HapColor::Rgb => BcColor::Rgb,
        HapColor::Rgba => BcColor::Rgba,
        HapColor::YCoCg => BcColor::YCoCg,
        HapColor::Alpha => BcColor::Alpha,
    };
    let bc = BcFrame {
        width: frame.width,
        height: frame.height,
        color,
        format,
        data: &frame.texture.data,
        alpha: frame.alpha.as_ref().map(|a| a.data.as_slice()),
    };
    converter.convert(gpu, &bc, target).map_err(CookError::from)
}
