//! `tex.ndi_in`: an NDI® source on the network as a texture (OBS, vMix, a camera, NDI Test
//! Patterns, another penumbra), at the source's size, through [`penumbra_ndi`]; its audio as
//! the second output. NDI® is a registered trademark of Vizrt NDI AB (<https://ndi.video/>).
//!
//! A thread of its own receives the source and wakes the node when a picture arrives, so the
//! picture stays live while the show is paused, as a camera's does. A picture that arrives
//! while the node is not wanted is replaced by the next one. Audio is kept only while the
//! `audio` output is read; it carries what arrived since the last cook, one channel per source
//! channel (`ch1`, `ch2`, ...) at the source's sample rate, as `chan.audio_in` does, and is
//! handed on unchanged when only a picture came (and the picture when only audio came), so
//! readers of the one do not recook for the other. Needs the NDI runtime (NDI Tools or the
//! NDI Runtime installer).

use penumbra_core::{Channel, Chans, Error, Name, ParamSpec};
use penumbra_gpu::{Alpha, TexRing};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, JobStatus, NodeDesc, NodeTypes, Operator,
};
use penumbra_ndi::{Bandwidth, Picture, Receiver, Unpacker};

use crate::out::spout::alpha_param;

/// The node kind.
pub const NDI_IN: &str = "tex.ndi_in";
const AUDIO: usize = 1;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            NDI_IN,
            "An NDI\u{ae} source on the network (needs the NDI runtime, ndi.video); resolution of the source's",
        )
        .param(ParamSpec::text("source", "").doc(
            "The source's name as `PC (Source)` or a part of it, e.g. `OBS` (the ... menu lists those on \
             the network); empty: the first source",
        ))
        .param(
            ParamSpec::choice("bandwidth", &["full", "low"])
                .doc("Which of the sender's streams to take")
                .choice_doc("full", "the stream as sent")
                .choice_doc("low", "a smaller, lower-quality stream the sender offers beside it, for previews"),
        )
        .param(alpha_param("How the source's transparency is taken (NDI's alpha is straight)"))
        .output("out", DataKind::Tex)
        .output("audio", DataKind::Chan)
        .lazy_output("audio")
        .describe_output("out", "The source's picture")
        .describe_output("audio", "The source's audio since the last cook: ch1, ch2, ... at its sample rate"),
        || Box::new(NdiIn::default()) as Box<dyn Operator>,
    ))
}

#[derive(Default)]
struct NdiIn {
    receiver: Option<((String, Bandwidth), Receiver)>,
    unpacker: Unpacker,
    ring: TexRing,
    /// The picture shown, kept to convert again when `alpha` changes.
    shown: Option<Picture>,
    out: Option<Data>,
    audio: Option<Data>,
    names: Vec<Name>,
    failed: Option<String>,
}

impl Operator for NdiIn {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let result = self.cook_inner(cx);
        self.failed = match &result {
            Err(CookError::Failed(e)) => Some(e.clone()),
            _ => None,
        };
        result
    }

    /// Running until the first picture (so `node.snapshot` waits for the source), idle after.
    fn job_status(&self) -> Option<JobStatus> {
        Some(match (&self.failed, self.out.is_some()) {
            (Some(e), false) => JobStatus::Failed { error: e.clone() },
            (None, false) => JobStatus::Running { progress: None },
            _ => JobStatus::Idle,
        })
    }
}

impl NdiIn {
    fn cook_inner(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = super::gpu(cx)?;
        let source = cx.text("source")?.trim().to_string();
        let bandwidth = if cx.text("bandwidth")? == "low" { Bandwidth::Lowest } else { Bandwidth::Highest };
        let key = (source.clone(), bandwidth);
        if self.receiver.as_ref().is_none_or(|(k, _)| *k != key) {
            let waker = cx.waker();
            let name = format!("penumbra {}", cx.def().name.as_str());
            let receiver =
                Receiver::new(&source, bandwidth, &name, move || waker.wake()).map_err(CookError::failed)?;
            self.receiver = Some((key, receiver));
            self.shown = None;
            self.out = None;
            self.audio = None;
        }
        let Some((_, receiver)) = self.receiver.as_ref() else {
            return Err(CookError::failed("no receiver"));
        };
        let want_audio = cx.output_wanted(AUDIO);
        receiver.want_audio(want_audio);
        let alpha = cx.text("alpha")?;
        let alpha =
            Alpha::parse(alpha).ok_or_else(|| CookError::failed(format!("unknown alpha `{alpha}`")))?;
        let mut changed = false;
        let picture = receiver.take_picture();
        let again = picture.is_none() && cx.params_changed() && self.shown.is_some();
        if picture.is_some() || again {
            if let Some(p) = picture
                && let Some(old) = self.shown.replace(p)
            {
                receiver.recycle(old);
            }
            if let Some(p) = &self.shown {
                let (w, h) = p.size();
                let target = self.ring.next(gpu, w, h, NDI_IN)?;
                self.unpacker.convert(gpu, p, alpha, &target).map_err(CookError::failed)?;
                self.out = Some(Data::new(DataKind::Tex, target));
                changed = true;
            }
        }
        if want_audio {
            let audio = receiver.take_audio();
            if audio.channels.first().is_some_and(|c| !c.is_empty()) && audio.rate > 0 {
                if self.names.len() != audio.channels.len() {
                    self.names = (1..=audio.channels.len())
                        .filter_map(|i| Name::new(&format!("ch{i}")).ok())
                        .collect();
                }
                let channels = self
                    .names
                    .iter()
                    .zip(audio.channels)
                    .map(|(name, samples)| Channel {
                        name: name.clone(),
                        samples: samples.into_iter().map(f64::from).collect(),
                    })
                    .collect();
                self.audio = Some(Data::new(DataKind::Chan, Chans::new(f64::from(audio.rate), channels)?));
                changed = true;
            }
        } else if self.audio.as_ref().is_none_or(|d| !d.is_skipped()) {
            self.audio = Some(Data::skipped(DataKind::Chan));
            changed = self.out.is_some();
        }
        let status = receiver.status();
        let Some(out) = self.out.clone() else {
            return Err(match status.error {
                Some(e) => CookError::failed(e),
                None => CookError::Pending,
            });
        };
        if let Some(e) = &status.error {
            cx.warn(e.clone());
        } else if !status.connected {
            cx.warn(format!(
                "{} is not connected: showing its last picture",
                status.source.as_deref().unwrap_or("the source")
            ));
        }
        if !changed {
            return Ok(Cooked::Unchanged);
        }
        let audio = match &self.audio {
            Some(a) => a.clone(),
            // Wanted, but nothing came yet: no channels.
            None => {
                let empty = Data::new(DataKind::Chan, Chans::new(48000.0, Vec::new())?);
                self.audio = Some(empty.clone());
                empty
            }
        };
        Ok(Cooked::Outputs(vec![out, audio]))
    }
}
