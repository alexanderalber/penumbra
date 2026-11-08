//! `tex.screen_in`: a monitor or a window of this machine as a texture, at its size, through
//! [`penumbra_screen`] (Windows.Graphics.Capture).
//!
//! A thread of its own finds the monitor or window and keeps the capture running; Windows wakes
//! the node when a new frame is there, so the picture stays live while the show is paused, as
//! a camera's does. Windows sends a frame only when something on the monitor or in the window
//! changed. When the window closes or the monitor goes, the node shows the last picture with a
//! warning and the thread looks for it again. Windows only.

use penumbra_core::{Error, ParamSpec};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, JobStatus, NodeDesc, NodeTypes, Operator,
};
use penumbra_screen::{Capture, Options, Target};

/// The node kind.
pub const SCREEN_IN: &str = "tex.screen_in";

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(SCREEN_IN, "A monitor or a window of this machine as a picture (Windows)")
            .block(penumbra_graph::Block::In)
            .param(
                ParamSpec::choice("source", &["monitor", "window"])
                    .doc("What to capture")
                    .choice_doc("monitor", "a whole monitor, chosen by `display`")
                    .choice_doc("window", "one window, chosen by a part of its title in `window`"),
            )
            .param(
                ParamSpec::text("display", "")
                    .doc(
                        "The monitor: empty for the primary one, 1, 2, ... in the order the ... menu and \
                         screen.sources list them, or a display id as out.projector takes it",
                    )
                    .applies_when("source", &["monitor"]),
            )
            .param(
                ParamSpec::text("window", "")
                    .doc(
                        "A part of the window's title, upper or lower case alike, e.g. `notepad`. The \
                         frontmost window that matches is captured; when it closes, the node waits for \
                         one to match again. The ... menu lists the windows open now",
                    )
                    .applies_when("source", &["window"]),
            )
            .param(ParamSpec::bool("cursor", false).doc(
                "Draw the mouse pointer into the picture. Before Windows 10 2004 the pointer is always drawn",
            ))
            .param(
                ParamSpec::bool("border", false).doc(
                    "Let Windows draw its yellow frame around what is captured. Windows 10 always draws it",
                ),
            )
            .output("out", DataKind::Tex)
            .describe_output("out", "The monitor or window, at its size in pixels"),
        || Box::new(ScreenIn::default()) as Box<dyn Operator>,
    ))
}

#[derive(Default)]
struct ScreenIn {
    /// Kept for the node's life; only its session changes with the params.
    capture: Option<Capture>,
    key: Option<(Target, Options)>,
    out: Option<Data>,
    failed: Option<String>,
}

impl Operator for ScreenIn {
    fn gpu_bytes(&self) -> u64 {
        self.capture.as_ref().map_or(0, Capture::bytes)
    }

    fn trim(&mut self) {
        if let Some(c) = self.capture.as_mut() {
            c.trim();
        }
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let result = self.cook_inner(cx);
        self.failed = match &result {
            Err(CookError::Failed(e)) => Some(e.clone()),
            _ => None,
        };
        result
    }

    /// Running until the first frame (so `node.snapshot` waits for it), idle after.
    fn job_status(&self) -> Option<JobStatus> {
        Some(match (&self.failed, self.out.is_some()) {
            (Some(e), false) => JobStatus::Failed { error: e.clone() },
            (None, false) => JobStatus::Running { progress: None },
            _ => JobStatus::Idle,
        })
    }
}

impl ScreenIn {
    fn cook_inner(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = super::gpu(cx)?;
        let target = match cx.text("source")? {
            "window" => Target::Window(cx.text("window")?.trim().to_string()),
            _ => Target::Monitor(cx.text("display")?.trim().to_string()),
        };
        let options = Options { cursor: cx.bool("cursor")?, border: cx.bool("border")? };
        let key = (target, options);
        if self.capture.is_none() {
            self.capture = Some(Capture::new(gpu).map_err(CookError::failed)?);
        }
        let Some(capture) = self.capture.as_mut() else { return Err(CookError::failed("no capture")) };
        if self.key.as_ref() != Some(&key) {
            self.out = None;
            let waker = cx.waker();
            capture.start(key.0.clone(), options, move || waker.wake()).map_err(CookError::failed)?;
            self.key = Some(key);
        }
        let taken = capture.take(gpu);
        let status = capture.status();
        if let Some(w) = &status.warning {
            cx.warn(w.clone());
        }
        match taken {
            Ok(Some(tex)) => {
                let data = Data::new(DataKind::Tex, tex);
                self.out = Some(data.clone());
                return Ok(Cooked::Outputs(vec![data]));
            }
            Ok(None) => {}
            Err(e) if self.out.is_none() => return Err(CookError::failed(e)),
            Err(e) => cx.warn(e),
        }
        if self.out.is_none() {
            if let Some(e) = status.error {
                return Err(CookError::failed(e));
            }
            if status.waiting {
                cx.warn("waiting for the first picture: Windows sends one when the window is shown (a minimised window sends none)");
            }
            return Err(CookError::Pending);
        }
        if let Some(e) = status.error {
            cx.warn(format!("{e}: showing the last picture"));
        }
        Ok(Cooked::Unchanged)
    }
}
