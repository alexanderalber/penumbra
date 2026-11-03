//! `out.ndi`: offers its input on the network as an NDI® source ([`penumbra_ndi`]); OBS,
//! vMix, Resolume, TouchDesigner, NDI Studio Monitor and other programs list it by name. NDI® is
//! a registered trademark of Vizrt NDI AB (<https://ndi.video/>).
//!
//! The node sends a frame whenever its input changes, announced at the show rate; receivers
//! keep the last one meanwhile. The conversion runs on the GPU and the sending on a thread of
//! its own; a frame that finds the readback ring full is left out. Needs the NDI runtime (NDI
//! Tools or the NDI Runtime installer); without it the node fails with where to get it.
//!
//! Sound wired to `audio` goes along: each new block (the block contract of
//! [`crate::audio::block`]) as planar float at its own rate, up to 16 channels. While sound is
//! wired, the node follows the show clock, and pictures and sound both carry the show time as
//! their NDI timecode, so a receiver can line them up. Without sound the runtime stamps the
//! pictures, as before.

use penumbra_core::time::TICKS_PER_SECOND;
use penumbra_core::{Chans, Error, ParamSpec, Tick, Value};
use penumbra_gpu::Alpha;
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};
use penumbra_ndi::Sender;
use penumbra_ndi::ffi::{MAX_SEND_CHANNELS, SEND_RATES};

use crate::out::spout::alpha_param;

/// The node kind.
pub const NDI_OUT: &str = "out.ndi";
/// Index of the sound input.
const AUDIO_INPUT: usize = 1;
/// Most of one input block that is sent, seconds (the newest part of a longer one).
const MAX_BLOCK_SECONDS: f64 = 1.0;
/// NDI timecodes count 100 ns steps.
const TIMECODE_PER_SECOND: i64 = 10_000_000;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    // Alpha doubles the sending CPU and adds about 10 ms (measured at 1080p60), and most
    // receivers ignore it, so this node sends none unless asked.
    let mut alpha = alpha_param(
        "How transparency travels to the receivers. With alpha, NDI sends an extra plane, which \
         costs about twice the CPU and some latency",
    );
    alpha.default = Value::Text("opaque".into());
    types.register(FnNodeType::new(
        NodeDesc::new(
            NDI_OUT,
            "Sends a picture, and sound if wired, over the network as an NDI\u{ae} source, to other programs \
             and computers; needs the NDI runtime (ndi.video)",
        )
        .param(ParamSpec::text("name", "").doc(
            "The source name receivers pick. NDI puts the machine's name before the source name, as in \
             `PC (name)`. Empty: `penumbra` and the node's name, such as `penumbra ndi1`",
        ))
        .param(alpha)
        .input("in", DataKind::Tex)
        .describe_input("in", "The picture to send")
        .input("audio", DataKind::Audio)
        .describe_input(
            "audio",
            "Sound to send with the picture: any rate from 8000 to 192000 samples per second, up to 16 \
             channels. Pictures and sound then carry the show time, so receivers keep them in step",
        ),
        || Box::new(NdiOut::default()) as Box<dyn Operator>,
    ))
}

#[derive(Default)]
struct NdiOut {
    sender: Option<Sender>,
    /// The name the sender was made for.
    wanted: String,
    /// Why making a sender failed: not tried again until a param changes.
    failed: Option<String>,
    /// The picture sent last ([`Data::address`]), so a cook for sound alone does not send it
    /// again.
    sent_picture: Option<usize>,
    /// Whether sound was wired at the last cook: the node then follows the show clock.
    sound: bool,
    /// The sound block sent last and the tick it came at.
    sent_sound: Option<(Data, Tick)>,
}

/// `tick` as an NDI timecode (100 ns steps), exact for any tick.
fn timecode(tick: Tick) -> i64 {
    let t = i128::from(tick.0) * i128::from(TIMECODE_PER_SECOND) / i128::from(TICKS_PER_SECOND);
    i64::try_from(t).unwrap_or(if t < 0 { i64::MIN + 1 } else { i64::MAX - 1 })
}

/// The block on the sound input as planar f32 at a whole rate, its newest second at most.
fn block(chans: &Chans) -> Result<(u32, Vec<Vec<f32>>), CookError> {
    let rate_f = chans.rate();
    let (lo, hi) = (*SEND_RATES.start(), *SEND_RATES.end());
    // `as`: checked to be a whole number in range first.
    let rate =
        if rate_f.is_finite() && rate_f.fract() == 0.0 && (f64::from(lo)..=f64::from(hi)).contains(&rate_f) {
            rate_f as u32
        } else {
            return Err(CookError::failed(format!(
                "the sound input is at {rate_f} samples per second; NDI sends whole rates from {lo} to {hi}"
            )));
        };
    let channels = chans.channels();
    if channels.len() > MAX_SEND_CHANNELS {
        return Err(CookError::failed(format!(
            "the sound input has {} channels; NDI sends at most {MAX_SEND_CHANNELS}",
            channels.len()
        )));
    }
    // `as`: seconds to a sample count.
    let max = (rate_f * MAX_BLOCK_SECONDS) as usize;
    let planes = channels
        .iter()
        .map(|c| {
            let skip = c.samples.len().saturating_sub(max);
            // `as`: samples to the f32 NDI carries (finite, as `Chans` keeps them).
            c.samples.iter().skip(skip).map(|s| *s as f32).collect()
        })
        .collect();
    Ok((rate, planes))
}

impl Operator for NdiOut {
    fn time_dependent(&self) -> Option<bool> {
        Some(self.sound)
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = crate::gpu(cx)?;
        let set = cx.text("name")?.trim();
        let wanted =
            if set.is_empty() { format!("penumbra {}", cx.def().name.as_str()) } else { set.to_string() };
        if wanted != self.wanted {
            self.sender = None;
            self.failed = None;
            self.sent_picture = None;
            self.wanted = wanted.clone();
        }
        if cx.params_changed() {
            self.failed = None;
            // A changed `alpha` sends the same picture anew.
            self.sent_picture = None;
        }
        self.sound = cx.def().inputs.get(AUDIO_INPUT).is_some_and(Option::is_some);
        if let Some(e) = &self.failed {
            return Err(CookError::failed(e.clone()));
        }
        if self.sender.is_none() {
            match Sender::new(&wanted) {
                Ok(s) => self.sender = Some(s),
                Err(e) => {
                    self.failed = Some(e.clone());
                    return Err(CookError::failed(e));
                }
            }
        }
        let Some(sender) = self.sender.as_mut() else { return Err(CookError::failed("no sender")) };
        if let Some(e) = sender.error() {
            if sender.name().is_none() {
                // The source could not be announced: the send thread has ended.
                self.sender = None;
                self.failed = Some(e.clone());
                return Err(CookError::failed(e));
            }
            cx.warn(e);
        }
        // Only a node with sound reads the clock (and is declared time-dependent for it).
        let now = self.sound.then(|| cx.time().tick);
        let stamp = now.map(timecode);
        // Sound first: a block ends now, so it belongs before this frame's picture.
        if let (Some(now), Some(data)) = (now, cx.input(AUDIO_INPUT)) {
            let Some(chans) = data.get::<Chans>() else {
                return Err(crate::waiting(cx));
            };
            let again = self.sent_sound.as_ref().is_some_and(|(d, t)| d.same(data) || *t == now);
            if !again {
                self.sent_sound = Some((data.clone(), now));
                let (rate, planes) = block(chans)?;
                let len = i64::try_from(planes.first().map_or(0, Vec::len)).unwrap_or(0);
                let start = stamp.map(|s| s.saturating_sub(len * TIMECODE_PER_SECOND / i64::from(rate)));
                sender.send_audio(rate, planes, start).map_err(CookError::failed)?;
            }
        } else {
            self.sent_sound = None;
        }
        let dropped = sender.dropped_audio();
        if dropped > 0 {
            cx.warn(format!("{dropped} blocks of sound left out: sending fell behind"));
        }
        let Some(picture) = cx.input(0) else {
            cx.warn("nothing connected: receivers see the last picture");
            return Ok(Cooked::Outputs(Vec::new()));
        };
        let Some(tex) = picture.get::<penumbra_gpu::Tex>() else {
            return Err(crate::waiting(cx));
        };
        if self.sent_picture == Some(picture.address()) {
            return Ok(Cooked::Outputs(Vec::new()));
        }
        let alpha = cx.text("alpha")?;
        let alpha =
            Alpha::parse(alpha).ok_or_else(|| CookError::failed(format!("unknown alpha `{alpha}`")))?;
        let rate = cx.rate();
        let sent =
            sender.send(gpu, tex, alpha, (rate.num(), rate.den()), stamp).map_err(CookError::failed)?;
        if sent == penumbra_ndi::Sent::Frame {
            self.sent_picture = Some(picture.address());
        }
        Ok(Cooked::Outputs(Vec::new()))
    }
}

#[cfg(test)]
mod tests {
    use penumbra_core::{Channel, Chans, Name};

    use super::*;

    fn chans(rate: f64, count: usize, len: usize) -> Chans {
        let channels = (0..count)
            .map(|i| Channel {
                name: Name::new(&format!("ch{}", i + 1)).unwrap(),
                samples: (0..len).map(|k| (k + i) as f64 / 10.0).collect(),
            })
            .collect();
        Chans::new(rate, channels).unwrap()
    }

    #[test]
    fn a_tick_becomes_100_ns_steps() {
        assert_eq!(timecode(Tick::ZERO), 0);
        assert_eq!(timecode(Tick(TICKS_PER_SECOND)), 10_000_000);
        assert_eq!(timecode(Tick(TICKS_PER_SECOND / 60)), 166_666);
        assert_eq!(timecode(Tick(-TICKS_PER_SECOND)), -10_000_000);
        assert!(timecode(Tick(i64::MAX)) > 0);
    }

    #[test]
    fn a_block_goes_planar() {
        let (rate, planes) = block(&chans(48_000.0, 2, 3)).unwrap();
        assert_eq!(rate, 48_000);
        assert_eq!(planes, vec![vec![0.0, 0.1, 0.2], vec![0.1, 0.2, 0.3]]);
    }

    #[test]
    fn a_long_block_keeps_its_newest_second() {
        let (_, planes) = block(&chans(8_000.0, 1, 8_100)).unwrap();
        assert_eq!(planes[0].len(), 8_000);
        assert!((planes[0][0] - 10.0).abs() < 1e-6, "{}", planes[0][0]);
    }

    #[test]
    fn odd_rates_and_too_many_channels_are_refused() {
        assert!(block(&chans(44_100.5, 1, 4)).is_err());
        assert!(block(&chans(4_000.0, 1, 4)).is_err());
        assert!(block(&chans(48_000.0, 17, 4)).is_err());
    }
}
