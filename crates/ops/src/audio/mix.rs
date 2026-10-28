//! `audio.mix`: adds its inputs, each by its own gain.
//!
//! The output has the channels of the first wired input. Every other input adds its channel of
//! the same name, else its first channel. Inputs must share one sample rate; nothing resamples.
//! Blocks of different length are lined up at their ends (the newest samples), so a source on a
//! device clock mixes with one on the show clock, though not to the sample. A source that hands
//! on the same block as before while another input brought a new one counts as silent for this
//! block, so a device that delivered nothing is not heard twice. A gain change glides across
//! the block.

use penumbra_core::{Channel, Chans, Error, ParamSpec};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

/// Kind of the mixer.
pub const MIX: &str = "audio.mix";
/// Inputs of the mixer, each with a gain param.
pub const MIX_INPUTS: usize = 8;

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
        .input_run_of(
            "in",
            DataKind::Audio,
            MIX_INPUTS,
            2,
            "The sounds to add, all at one sample rate. The output has the channels of the first wired one",
        )
        .output("out", DataKind::Audio)
        .sampled();
    types.register(FnNodeType::new(desc, || Box::new(Mix::default()) as Box<dyn Operator>))
}

#[derive(Default)]
struct Mix {
    /// The gain of each input and the master gain as the last block ended.
    gains: Option<[f64; MIX_INPUTS + 1]>,
    /// The result each input handed on last, kept so a new one cannot take its address.
    seen: Vec<Option<Data>>,
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
        self.seen.resize(MIX_INPUTS, None);
        let seen = |i: usize, d: &Data| self.seen.get(i).and_then(Option::as_ref).is_some_and(|s| s.same(d));
        let Some(&(first_i, first, _)) = inputs.first() else {
            return Err(crate::waiting_for(cx, "connect sound to in0"));
        };
        for &(i, c, _) in &inputs {
            if c.rate() != first.rate() {
                return Err(CookError::failed(format!(
                    "in{i} is at {} Hz, in{first_i} at {} Hz: audio.mix does not resample, so give both the same rate",
                    c.rate(),
                    first.rate()
                )));
            }
        }
        let mut target = [1.0; MIX_INPUTS + 1];
        for (i, g) in target.iter_mut().take(MIX_INPUTS).enumerate() {
            *g = cx.float(&format!("gain{i}"))?;
        }
        target[MIX_INPUTS] = cx.float("gain")?;
        let from = self.gains.unwrap_or(target);
        // When some input brought a new block, one that did not is silent for it.
        let any_new = inputs.iter().any(|&(i, _, d)| !seen(i, d));
        let len = first.len();
        let mut out: Vec<Channel> = first
            .channels()
            .iter()
            .map(|c| Channel { name: c.name.clone(), samples: vec![0.0; len] })
            .collect();
        for &(i, c, d) in &inputs {
            if any_new && seen(i, d) {
                continue;
            }
            let (g0, g1) = (from.get(i).copied().unwrap_or(1.0), target.get(i).copied().unwrap_or(1.0));
            for o in &mut out {
                let Some(src) = c.get(o.name.as_str()).or_else(|| c.channels().first()) else { continue };
                // Lined up at the ends.
                let n = src.samples.len().min(len);
                let skip_src = src.samples.len() - n;
                let skip_out = len - n;
                for (k, (d, s)) in
                    o.samples.iter_mut().skip(skip_out).zip(src.samples.iter().skip(skip_src)).enumerate()
                {
                    let t = (skip_out + k) as f64 / len as f64;
                    *d += s * (g0 + (g1 - g0) * t);
                }
            }
        }
        for o in &mut out {
            for (k, d) in o.samples.iter_mut().enumerate() {
                let t = k as f64 / len as f64;
                *d *= from[MIX_INPUTS] + (target[MIX_INPUTS] - from[MIX_INPUTS]) * t;
            }
        }
        self.gains = Some(target);
        let mut now = vec![None; MIX_INPUTS];
        for &(i, _, d) in &inputs {
            if let Some(slot) = now.get_mut(i) {
                *slot = Some(d.clone());
            }
        }
        self.seen = now;
        let chans = Chans::new(first.rate(), out)?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Audio, chans)]))
    }
}
