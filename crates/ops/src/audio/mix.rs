//! `audio.mix`: adds its inputs, each by its own gain, at one sample rate.
//!
//! - **Clock and rate:** the first wired input leads. The mix makes one block whenever that
//!   input brings a new one, as long as that block (converted to the mix's rate when `rate`
//!   asks for another). The output has the first input's channels; every other input adds its
//!   channel of the same name, else its first channel.
//! - **In step:** an input at the mix's rate whose blocks arrive with the first input's and
//!   match them in length (two generators on the show clock) is added sample for sample, with
//!   no delay. Its first block decides; an input that falls out of step later moves to a queue
//!   for good.
//! - **Queued:** every other input (another rate, a device on its own clock) is converted to
//!   the mix's rate by a [`penumbra_audio::Resampler`] that lives across cooks, and queued
//!   ([`queue`]). Each new block of the first input takes as many samples from each queue.
//!   The queue absorbs the jitter of a device and follows its clock's drift. Gaps and drops
//!   fade, are counted and shown as a warning for [`WARN_SECONDS`].
//! - **Repeats:** when the first input hands on the same block as before (a device that
//!   delivered nothing), the mix hands on its last block too; new blocks of the other inputs
//!   are queued for later. At the same tick after a param change, the last block is mixed again
//!   with the new gains, and with the new block of an input in step that was cooked again. A
//!   gain change glides across the block.

pub mod queue;

use penumbra_core::{Channel, Chans, Error, Name, ParamSpec, Tick};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

use queue::Queue;

use super::oscillator::{MAX_RATE, MIN_RATE};

/// Kind of the mixer.
pub const MIX: &str = "audio.mix";
/// Inputs of the mixer, each with a gain param.
pub const MIX_INPUTS: usize = 8;
/// How long a gap or a drop stays shown as a warning, seconds of sound.
pub const WARN_SECONDS: f64 = 2.0;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    let mut desc = NodeDesc::new(MIX, "Adds sounds together, each by its own gain").param(
        ParamSpec::float("gain", 1.0)
            .range(0.0, 2.0)
            .doc("Multiplies the sum of all inputs: the level of the whole mix"),
    );
    for i in 0..MIX_INPUTS {
        let mut spec = ParamSpec::float(&format!("gain{i}"), 1.0)
            .range(0.0, 2.0)
            .doc(&format!("Multiplies input in{i} before it is added; 0 mutes it"));
        if i >= 2 {
            spec = spec.more();
        }
        desc = desc.param(spec);
    }
    let desc = desc
        .param(ParamSpec::int("rate", 0).clamp(0.0, f64::from(MAX_RATE)).more().doc(
            "Samples per second of the mix. 0 takes the rate of the first wired input. Inputs at another rate are converted to it",
        ))
        .input_run_of(
            "in",
            DataKind::Audio,
            MIX_INPUTS,
            2,
            "The sounds to add, at any sample rate. The first wired one sets the channels, and the rate when rate is 0",
        )
        .output("out", DataKind::Audio)
        .sampled()
        .time_dependent();
    types.register(FnNodeType::new(desc, || Box::new(Mix::default()) as Box<dyn Operator>))
}

/// One input other than the first.
struct Lane {
    rate: f64,
    names: Vec<Name>,
    /// None: in step, added as it comes.
    queue: Option<Queue>,
    seen: Option<Data>,
    /// The tick its last block came at.
    tick: Option<Tick>,
    /// Gaps and drops counted so far, and the samples of sound the warning stays.
    counted: (u64, u64),
    warn_left: usize,
}

/// What the mix is set up for; another starts everything over.
#[derive(Clone, PartialEq)]
struct Setup {
    first: usize,
    first_rate: f64,
    first_names: Vec<Name>,
    rate: f64,
}

/// The last block made.
struct Last {
    first: Data,
    out: Data,
    tick: Tick,
    /// What each input gave to it, per its own channels.
    takes: Vec<(usize, Vec<Vec<f64>>)>,
    /// The gains at the start of the block.
    from: [f64; MIX_INPUTS + 1],
}

#[derive(Default)]
struct Mix {
    setup: Option<Setup>,
    /// Converts the first input when `rate` asks for another rate than its own.
    convert: Option<penumbra_audio::Resampler>,
    lanes: Vec<Option<Lane>>,
    last: Option<Last>,
    /// The gains as the last block ended.
    gains: Option<[f64; MIX_INPUTS + 1]>,
}

fn names(c: &Chans) -> Vec<Name> {
    c.channels().iter().map(|c| c.name.clone()).collect()
}

fn slices(c: &Chans) -> Vec<&[f64]> {
    c.channels().iter().map(|c| c.samples.as_slice()).collect()
}

impl Operator for Mix {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let mut inputs: Vec<(usize, &Chans, &Data)> = Vec::new();
        for i in 0..MIX_INPUTS {
            if let Some(d) = cx.input(i)
                && let Some(c) = d.get::<Chans>()
            {
                inputs.push((i, c, d));
            }
        }
        let Some(&(first_i, first, first_data)) = inputs.first() else {
            *self = Mix::default();
            return Err(crate::waiting_for(cx, "connect sound to in0"));
        };
        let asked = cx.int("rate")?;
        let rate = if asked <= 0 {
            first.rate()
        } else {
            // `as`: clamped to the supported rates first.
            asked.clamp(i64::from(MIN_RATE), i64::from(MAX_RATE)) as f64
        };
        let setup = Setup { first: first_i, first_rate: first.rate(), first_names: names(first), rate };
        if self.setup.as_ref() != Some(&setup) {
            let mut fresh = Mix { setup: Some(setup), lanes: Vec::new(), ..Mix::default() };
            if first.rate() != rate {
                fresh.convert = Some(
                    penumbra_audio::Resampler::new(first.rate(), rate, first.channels().len().max(1))
                        .map_err(|e| CookError::failed(format!("in{first_i}: {e}")))?,
                );
            }
            *self = fresh;
        }
        self.lanes.resize_with(MIX_INPUTS, || None);
        // Inputs no longer wired start over when they come back.
        for (i, lane) in self.lanes.iter_mut().enumerate() {
            if !inputs.iter().any(|&(j, _, _)| j == i) || i == first_i {
                *lane = None;
            }
        }
        let mut target = [1.0; MIX_INPUTS + 1];
        for (i, g) in target.iter_mut().take(MIX_INPUTS).enumerate() {
            *g = cx.float(&format!("gain{i}"))?;
        }
        target[MIX_INPUTS] = cx.float("gain")?;
        let tick = cx.time().tick;
        let others = inputs.get(1..).unwrap_or(&[]);

        let repeat = self.last.as_ref().is_some_and(|l| l.first.same(first_data));
        if repeat {
            // The first input brought nothing: the others' new blocks wait in their queues. A
            // new block at the tick of an input's last one replaces that one (a param changed
            // while the show is paused): added in step, it goes into the block again; queued, it
            // is left out, since its samples were queued already.
            let mut replaced = false;
            for &(i, c, d) in others {
                replaced |= self.feed_late(i, c, d, rate, tick);
            }
            let Some(last) = &self.last else {
                return Err(CookError::failed("audio.mix lost its last block"));
            };
            if last.tick != tick || !(cx.params_changed() || replaced) {
                return Ok(Cooked::Outputs(vec![last.out.clone()]));
            }
            let from = last.from;
            let chans = compose(first, &last.takes, &self.lanes, first_i, rate, from, target)?;
            let out = Data::new(DataKind::Audio, chans);
            if let Some(l) = self.last.as_mut() {
                l.out = out.clone();
            }
            self.gains = Some(target);
            return Ok(Cooked::Outputs(vec![out]));
        }

        // A new block of the first input: it sets the length.
        let lead: Vec<Vec<f64>> = match self.convert.as_mut() {
            Some(r) => {
                let mut out = vec![Vec::new(); r.channels()];
                r.process(&slices(first), &mut out)
                    .map_err(|e| CookError::failed(format!("in{first_i}: {e}")))?;
                out
            }
            None => first.channels().iter().map(|c| c.samples.clone()).collect(),
        };
        let len = lead.first().map_or(0, Vec::len);
        let mut takes = vec![(first_i, lead)];
        let mut warnings = Vec::new();
        for &(i, c, d) in others {
            let Some(slot) = self.lanes.get_mut(i) else { continue };
            if slot.as_ref().is_some_and(|l| l.rate != c.rate() || l.names != names(c)) {
                *slot = None;
            }
            let is_new = slot.as_ref().is_none_or(|l| l.seen.as_ref().is_none_or(|s| !s.same(d)));
            let lane = match slot {
                Some(l) => l,
                None => {
                    let in_step = c.rate() == rate && c.len() == len;
                    let queue = if in_step {
                        None
                    } else {
                        match new_queue(i, c, rate, Some(len)) {
                            Ok(q) => Some(q),
                            Err(e) => {
                                warnings.push(format!("{e}: left out"));
                                continue;
                            }
                        }
                    };
                    slot.insert(Lane {
                        rate: c.rate(),
                        names: names(c),
                        queue,
                        seen: None,
                        tick: None,
                        counted: (0, 0),
                        warn_left: 0,
                    })
                }
            };
            if lane.queue.is_none() && !(is_new && c.len() == len) {
                // Out of step from now on.
                match new_queue(i, c, rate, Some(len)) {
                    Ok(q) => lane.queue = Some(q),
                    Err(e) => {
                        warnings.push(format!("{e}: left out"));
                        continue;
                    }
                }
            }
            lane.seen = Some(d.clone());
            lane.tick = Some(tick);
            let take = match lane.queue.as_mut() {
                None => c.channels().iter().map(|ch| ch.samples.clone()).collect(),
                Some(q) => {
                    if is_new && let Err(e) = q.push(&slices(c)) {
                        warnings.push(format!("in{i}: {e}"));
                    }
                    let take = q.take(len);
                    let counted = (q.underruns, q.overruns);
                    if counted != lane.counted {
                        // `as`: a count of samples.
                        lane.warn_left = (WARN_SECONDS * rate) as usize;
                        lane.counted = counted;
                    }
                    if lane.warn_left > 0 {
                        warnings.push(format!(
                            "in{i}: {} gaps, {} drops so far (queue {:.0} ms)",
                            counted.0,
                            counted.1,
                            q.latency_seconds() * 1000.0
                        ));
                    }
                    lane.warn_left = lane.warn_left.saturating_sub(len);
                    take
                }
            };
            takes.push((i, take));
        }
        for w in warnings {
            cx.warn(w);
        }
        let from = self.gains.unwrap_or(target);
        let chans = compose(first, &takes, &self.lanes, first_i, rate, from, target)?;
        let out = Data::new(DataKind::Audio, chans);
        self.gains = Some(target);
        self.last = Some(Last { first: first_data.clone(), out: out.clone(), tick, takes, from });
        Ok(Cooked::Outputs(vec![out]))
    }
}

impl Mix {
    /// Queues a new block of input `i` that came while the first input brought none. Returns
    /// whether it replaced its block in the last mix.
    fn feed_late(&mut self, i: usize, c: &Chans, d: &Data, rate: f64, tick: Tick) -> bool {
        let Some(Some(lane)) = self.lanes.get_mut(i) else {
            // Seen for the first time: it joins with the first input's next block.
            return false;
        };
        if lane.rate != c.rate() || lane.names != names(c) || lane.seen.as_ref().is_some_and(|s| s.same(d)) {
            return false;
        }
        if lane.tick == Some(tick) {
            lane.seen = Some(d.clone());
            if lane.queue.is_some() {
                return false;
            }
            let Some(take) = self.last.as_mut().and_then(|l| l.takes.iter_mut().find(|(j, _)| *j == i))
            else {
                return false;
            };
            if take.1.first().map_or(0, Vec::len) != c.len() {
                return false;
            }
            take.1 = c.channels().iter().map(|ch| ch.samples.clone()).collect();
            return true;
        }
        if lane.queue.is_none() {
            match new_queue(i, c, rate, None) {
                Ok(q) => lane.queue = Some(q),
                Err(e) => {
                    log::warn!("audio.mix: {e}");
                    return false;
                }
            }
        }
        lane.seen = Some(d.clone());
        lane.tick = Some(tick);
        if let Some(q) = lane.queue.as_mut()
            && let Err(e) = q.push(&slices(c))
        {
            log::warn!("audio.mix: in{i}: {e}");
        }
        false
    }
}

/// A queue for input `i`, whose first block came with one of `len` samples of the first input
/// (or alone). Its first guess of the swing: a sample or two when both blocks match in
/// length after the conversion (two sources on the show clock), else the longer block.
fn new_queue(i: usize, c: &Chans, rate: f64, len: Option<usize>) -> Result<Queue, String> {
    // `as`: a block's length in samples at the mix's rate.
    let arriving = c.len() as f64 * rate / c.rate();
    let swing = match len {
        Some(l) if (arriving - l as f64).abs() <= 2.0 => 2.0,
        Some(l) => arriving.max(l as f64),
        None => arriving,
    };
    Queue::new(c.rate(), rate, c.channels().len().max(1), swing).map_err(|e| format!("in{i}: {e}"))
}

/// Adds what each input gave, by its gain gliding from `from` to `to`, onto the first input's
/// channels.
fn compose(
    first: &Chans,
    takes: &[(usize, Vec<Vec<f64>>)],
    lanes: &[Option<Lane>],
    first_i: usize,
    rate: f64,
    from: [f64; MIX_INPUTS + 1],
    to: [f64; MIX_INPUTS + 1],
) -> Result<Chans, CookError> {
    let len = takes.first().and_then(|(_, t)| t.first()).map_or(0, Vec::len);
    let first_names = names(first);
    let mut out: Vec<Channel> =
        first_names.iter().map(|n| Channel { name: n.clone(), samples: vec![0.0; len] }).collect();
    let ramp = |k: usize, a: f64, b: f64| {
        let t = k as f64 / len.max(1) as f64;
        a + (b - a) * t
    };
    for (i, take) in takes {
        let input_names: &[Name] = if *i == first_i {
            &first_names
        } else {
            match lanes.get(*i).and_then(Option::as_ref) {
                Some(l) => &l.names,
                None => continue,
            }
        };
        let (g0, g1) = (from.get(*i).copied().unwrap_or(1.0), to.get(*i).copied().unwrap_or(1.0));
        for o in &mut out {
            let at = input_names.iter().position(|n| *n == o.name).unwrap_or(0);
            let Some(src) = take.get(at) else { continue };
            for (k, (d, s)) in o.samples.iter_mut().zip(src).enumerate() {
                *d += s * ramp(k, g0, g1);
            }
        }
    }
    let (m0, m1) = (from[MIX_INPUTS], to[MIX_INPUTS]);
    for o in &mut out {
        for (k, d) in o.samples.iter_mut().enumerate() {
            *d *= ramp(k, m0, m1);
        }
    }
    Ok(Chans::new(rate, out)?)
}
