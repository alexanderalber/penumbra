//! `tex.video_in`: a live camera (a capture card such as a Cam Link, a webcam), read by the
//! engine's camera [`Hub`] on a thread of its own; the newest frame's planes are converted to a
//! texture on the GPU, as video files are.
//!
//! The node recooks when a frame arrives, not on the show clock, so the picture stays live while
//! the show is paused. A frame that arrives while the node is not wanted is simply replaced by
//! the next one: a live picture shows the freshest frame, never a backlog. A frame identical to
//! the one shown (a capture card repeating each frame of a 30p source on a 60 Hz signal) is
//! skipped, so the node's output changes only with the picture.
//!
//! The `info` output carries channels about the stream, updated with every frame: `fps`
//! (arriving, measured), `delay_ms` (the camera's timestamp to the capture thread), `age_ms`
//! (the capture thread to this cook), `frames` and `dropped` (lost before they reached
//! penumbra, by the camera's timestamps).

use std::sync::Arc;

use penumbra_camera::{CameraFrame, Hub, Key, Matrix, Watch};
use penumbra_core::{Channel, Chans, Error, Name, ParamSpec};
use penumbra_gpu::video::{Converter, Planes, Transfer, VideoColor, VideoPlanes};
use penumbra_gpu::{Gpu, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, JobStatus, Network, NodeDesc, NodeTypes,
    Operator,
};

use crate::on_gpu;

/// The node kind.
pub const VIDEO_IN: &str = "tex.video_in";

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(VIDEO_IN, "A live camera or capture card; resolution of the camera's mode")
            .param(ParamSpec::text("device", "").doc(
                "Part of the camera's name (the ... menu lists them), e.g. `Cam Link`; empty: the first camera",
            ))
            .param(ParamSpec::text("mode", "").doc(
                "Size, rate and pixel format, each optional, e.g. `1920x1080@60 NV12` or `@30`; empty: the \
                 largest, fastest the camera offers",
            ))
            .param(
                ParamSpec::choice("range", &["auto", "limited", "full"])
                    .more()
                    .doc("Whether black is 16 or 0 in the camera's Y'CbCr. Raised blacks mean the camera sends full range while limited is assumed")
                    .choice_doc("auto", "believe the camera (limited when it does not say)")
                    .choice_doc("limited", "black is 16, white 235 (video range)")
                    .choice_doc("full", "black is 0, white 255"),
            )
            .output("out", DataKind::Tex)
            .output("info", DataKind::Chan)
            .describe_output("out", "The camera's picture")
            .describe_output("info", "The stream: fps, delay_ms, age_ms, frames, dropped"),
        || Box::new(VideoIn::default()) as Box<dyn Operator>,
    )
    .warm_up(on_gpu(|gpu| gpu.video_pipeline().map(|_| ()))))
}

/// The streams the network's `tex.video_in` nodes want (stored params).
pub fn wants(net: &Network, types: &NodeTypes) -> Vec<Key> {
    let Some(desc) = types.desc(VIDEO_IN) else { return Vec::new() };
    let text = |n: &penumbra_graph::NodeDef, p: &str| {
        n.param(desc, p).and_then(|v| v.as_str()).map(str::to_string).unwrap_or_default()
    };
    let mut keys: Vec<Key> = net
        .nodes()
        .filter(|n| n.kind == VIDEO_IN)
        .map(|n| Key::new(&text(n, "device"), &text(n, "mode")))
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

#[derive(Default)]
struct VideoIn {
    watch: Option<Watch>,
    converter: Converter,
    ring: TexRing,
    /// The frame last shown (compared by identity, not by number: a stream that is closed and
    /// opened again counts from 1 again).
    shown: Option<Arc<CameraFrame>>,
    has_output: bool,
    failed: Option<String>,
}

impl Operator for VideoIn {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let result = self.cook_inner(cx);
        self.failed = match &result {
            Err(CookError::Failed(e)) => Some(e.clone()),
            _ => None,
        };
        result
    }

    /// Running until the first frame (so `node.snapshot` waits for the camera), idle after.
    fn job_status(&self) -> Option<JobStatus> {
        Some(match (&self.failed, self.has_output) {
            (Some(e), false) => JobStatus::Failed { error: e.clone() },
            (None, false) => JobStatus::Running { progress: None },
            _ => JobStatus::Idle,
        })
    }
}

fn same_picture(a: &CameraFrame, b: &CameraFrame) -> bool {
    (a.width, a.height, a.format, a.shift) == (b.width, b.height, b.format, b.shift)
        && a.y == b.y
        && a.cb == b.cb
        && a.cr == b.cr
}

impl VideoIn {
    fn cook_inner(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let hub = cx
            .resource::<Hub>()
            .ok_or_else(|| CookError::failed("camera input is not available in this process"))?;
        let key = Key::new(cx.text("device")?, cx.text("mode")?);
        if self.watch.as_ref().is_none_or(|w| *w.key() != key) {
            let waker = cx.waker();
            self.watch = Some(hub.watch(&key, Arc::new(move || waker.wake())));
            self.shown = None;
        }
        if let Some(w) = &self.watch {
            w.ack();
        }
        let Some(status) = hub.stream_status(&key) else {
            return Err(if hub.config().enabled {
                CookError::Pending
            } else {
                CookError::failed("camera input is switched off (video_in.config enabled)")
            });
        };
        let frame = hub.latest(&key);
        let new = frame.as_ref().is_some_and(|f| self.shown.as_ref().is_none_or(|s| !Arc::ptr_eq(s, f)));
        let changed = cx.params_changed() && self.has_output;
        // A repeat of the frame shown, byte for byte, is no new picture: a capture card fed 30p
        // over a 60 Hz signal delivers every frame twice. Passed on, it would make nodes that
        // compare frames (motion, averages) see no change every other frame.
        if let (Some(f), Some(s), true, false) = (&frame, &self.shown, new, changed)
            && self.has_output
            && same_picture(f, s)
        {
            self.shown = Some(f.clone());
            return Ok(Cooked::Unchanged);
        }
        match frame {
            Some(f) if new || changed => {
                let gpu = super::gpu(cx)?;
                let target = self.ring.next(gpu, f.width, f.height, VIDEO_IN)?;
                let full = match cx.text("range")? {
                    "limited" => false,
                    "full" => true,
                    _ => f.full_range,
                };
                convert(gpu, &mut self.converter, &f, full, &target)?;
                let age = f.arrived.elapsed().as_secs_f64() * 1000.0;
                let values = [
                    ("fps", status.fps),
                    ("delay_ms", status.delay_ms.unwrap_or(0.0)),
                    ("age_ms", age),
                    ("frames", status.frames as f64),
                    ("dropped", status.dropped as f64),
                ];
                let mut channels = Vec::with_capacity(values.len());
                for (name, v) in values {
                    channels.push(Channel { name: Name::new(name)?, samples: vec![v] });
                }
                let info = Chans::new(cx.rate().as_f64(), channels)?;
                self.shown = Some(f);
                self.has_output = true;
                if let (false, Some(e)) = (status.open, status.error) {
                    cx.warn(e);
                }
                Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, target), Data::new(DataKind::Chan, info)]))
            }
            _ => match (status.open, status.error) {
                (false, Some(e)) if self.has_output => {
                    cx.warn(e);
                    Ok(Cooked::Unchanged)
                }
                (false, Some(e)) => Err(CookError::failed(e)),
                _ if self.has_output => Ok(Cooked::Unchanged),
                _ => Err(CookError::Pending),
            },
        }
    }
}

fn convert(
    gpu: &Gpu,
    converter: &mut Converter,
    f: &CameraFrame,
    full_range: bool,
    target: &penumbra_gpu::Tex,
) -> Result<(), CookError> {
    let color = VideoColor {
        kr_kb: match f.matrix {
            Matrix::Bt601 => (0.299, 0.114),
            Matrix::Bt709 => (0.2126, 0.0722),
        },
        full_range,
        transfer: Transfer::Srgb,
    };
    let planes = VideoPlanes {
        width: f.width,
        height: f.height,
        shift: f.shift,
        planes: Planes::Yuv8 { planes: [f.y.as_slice(), f.cb.as_slice(), f.cr.as_slice()], alpha: None },
        color,
    };
    converter.convert(gpu, &planes, target).map_err(CookError::from)
}
