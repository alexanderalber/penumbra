//! `tex.spout_in`: another program's Spout sender as a texture (TouchDesigner, Resolume, OBS,
//! another penumbra), at the sender's size, through [`penumbra_spout`].
//!
//! A thread watches the sender and wakes the node when a frame arrives (by the sender's frame
//! counter where it keeps one, else at a fixed short interval), so the picture stays live while
//! the show is paused, as a camera's does. A frame the sender has not counted as new is not
//! copied again. Windows only.

use penumbra_core::{Error, ParamSpec};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, JobStatus, NodeDesc, NodeTypes, Operator,
};
use penumbra_spout::{Alpha, Received, Receiver, Watch};

use crate::out::spout::alpha_param;

/// The node kind.
pub const SPOUT_IN: &str = "tex.spout_in";

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(SPOUT_IN, "Another program's Spout sender (Windows); resolution of the sender's")
            .param(ParamSpec::text("sender", "").doc(
                "The sender's name (the ... menu lists those running); empty: the active sender, the one \
                 picked last in a Spout program, else the first",
            ))
            .param(alpha_param("How the sender's transparency is taken"))
            .output("out", DataKind::Tex)
            .describe_output("out", "The sender's picture"),
        || Box::new(SpoutIn::default()) as Box<dyn Operator>,
    ))
}

#[derive(Default)]
struct SpoutIn {
    receiver: Option<Receiver>,
    watch: Option<(String, Watch)>,
    has_output: bool,
    failed: Option<String>,
}

impl Operator for SpoutIn {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let result = self.cook_inner(cx);
        self.failed = match &result {
            Err(CookError::Failed(e)) => Some(e.clone()),
            _ => None,
        };
        result
    }

    /// Running until the first frame (so `node.snapshot` waits for the sender), idle after.
    fn job_status(&self) -> Option<JobStatus> {
        Some(match (&self.failed, self.has_output) {
            (Some(e), false) => JobStatus::Failed { error: e.clone() },
            (None, false) => JobStatus::Running { progress: None },
            _ => JobStatus::Idle,
        })
    }
}

impl SpoutIn {
    fn cook_inner(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = super::gpu(cx)?;
        let name = cx.text("sender")?.trim().to_string();
        if self.watch.as_ref().is_none_or(|(n, _)| *n != name) {
            let waker = cx.waker();
            self.watch = Some((name.clone(), Watch::new(&name, move || waker.wake())));
        }
        if self.receiver.is_none() {
            self.receiver = Some(Receiver::new(gpu).map_err(CookError::failed)?);
        }
        let Some(receiver) = self.receiver.as_mut() else { return Err(CookError::failed("no receiver")) };
        let alpha = Alpha::parse(cx.text("alpha")?).unwrap_or_default();
        let again = cx.params_changed() || !self.has_output;
        match receiver.receive(gpu, &name, alpha, again) {
            Ok(Received::Frame(tex)) => {
                self.has_output = true;
                Ok(Cooked::Outputs(vec![Data::new(DataKind::Tex, tex)]))
            }
            Ok(Received::Unchanged) if self.has_output => Ok(Cooked::Unchanged),
            Ok(Received::Unchanged) => Err(CookError::Pending),
            Err(e) => {
                self.has_output = false;
                Err(CookError::failed(e))
            }
        }
    }
}
