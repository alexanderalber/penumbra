//! `swarm` for the `drone` and `pad` presets of `audio.ambience`: each tone becomes a cloud of
//! [`SUBS`] voices whose pitches wander around its note.
//!
//! Every voice's wander is smooth gradient noise of the sample number, one seed per lane and
//! voice, with its own period between 2 and 6 seconds. The wander spans `swarm` squared times
//! [`SWARM_OCTAVES`] either side of the note: small values give a chorus, 1 a cloud half an
//! octave either side. The tone's own voice wanders too; the seven more fade in as `swarm`
//! rises from 0 to [`SWARM_FULL`], and the tone's own voice gets quieter by as much, so the
//! loudness holds. At `swarm` 0 none of this runs and the tone is exactly what it was.
//!
//! The voices' frequency factors are worked out at control points [`CONTROL`] samples apart on
//! the absolute sample number, with the wander's span at the segment's first sample, and moved
//! along in a straight line between them. That keeps 128 lanes of eight voices cheap. While
//! `swarm` moves, the span steps once per segment, phase-continuous: under 10 cents per step
//! at the default glide.

use std::f64::consts::TAU;

use crate::chan::{hash, noise as smooth};

/// Voices per tone while `swarm` is above 0: the tone's own voice and seven more.
pub(super) const SUBS: usize = 8;
/// The wander at `swarm` 1, octaves either side of the note.
pub(super) const SWARM_OCTAVES: f64 = 0.5;
/// The `swarm` from which the seven extra voices sound fully.
pub(super) const SWARM_FULL: f64 = 0.25;
/// Samples between the wanders' control points.
const CONTROL: i64 = 64;

/// One tone's extra voices and its cached wanders.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Swarm {
    /// The extra voices' phases, in waves.
    pub(super) phase: [f64; SUBS - 1],
    /// The control segment the wanders below belong to, and the rate they were made for.
    seg: Option<(i64, u64)>,
    /// Each voice's control values for the segment.
    voices: [Control; SUBS],
}

/// One voice's wander at the end of the control segment, and its frequency factor at the
/// segment's start and change per sample.
#[derive(Clone, Copy, Debug, Default)]
struct Control {
    end: f64,
    factor: f64,
    step: f64,
}

/// Voice `k`'s wander at sample `t`, about -1..1: two gradient noises at unrelated rates, so it
/// does not come back to the note at regular times.
fn wander(t: i64, k: usize, lane: usize, rate: f64) -> f64 {
    let seed = 50_021 + 977 * lane as i64 + 31 * k as i64;
    let period = 2.0 + 4.0 * f64::from(hash(k as i64, seed) >> 8) / f64::from(1u32 << 24);
    let x = t as f64 / (rate * period);
    let a = smooth(x, seed);
    let b = smooth(x * 2.71 + 0.37, seed.wrapping_add(1));
    ((a + 0.5 * b) / 1.2).clamp(-1.0, 1.0)
}

impl Swarm {
    pub(super) fn new(lane: usize, start: i64) -> Self {
        let mut phase = [0.0; SUBS - 1];
        for (k, p) in phase.iter_mut().enumerate() {
            *p = (super::super::oscillator::noise(start, 61 + k as i64 + 13 * lane as i64) + 1.0) / 2.0;
        }
        Self { phase, ..Self::default() }
    }

    /// Every voice's frequency factor at sample `t`, for a wander of `depth` octaves either
    /// side (taken at the first sample of each segment).
    pub(super) fn factors(&mut self, t: i64, lane: usize, rate: f64, depth: f64) -> [f64; SUBS] {
        let seg = t.div_euclid(CONTROL);
        let key = Some((seg, rate.to_bits()));
        if self.seg != key {
            let t0 = seg.saturating_mul(CONTROL);
            let t1 = t0.saturating_add(CONTROL);
            let next = self.seg == Some((seg.wrapping_sub(1), rate.to_bits()));
            for (k, v) in self.voices.iter_mut().enumerate() {
                let start = if next { v.end } else { wander(t0, k, lane, rate) };
                v.end = wander(t1, k, lane, rate);
                v.factor = (depth * start).exp2();
                v.step = ((depth * v.end).exp2() - v.factor) / CONTROL as f64;
            }
            self.seg = key;
        }
        let j = (t - seg.saturating_mul(CONTROL)) as f64;
        let mut out = [0.0; SUBS];
        for (o, v) in out.iter_mut().zip(&self.voices) {
            *o = v.factor + v.step * j;
        }
        out
    }
}

/// How far the voices wander at `swarm` `s`, octaves either side, and the gains of the tone's
/// own voice and of each extra voice.
pub(super) fn mix(s: f64) -> (f64, f64, f64) {
    let depth = SWARM_OCTAVES * s * s;
    let x = (s / SWARM_FULL).clamp(0.0, 1.0);
    let g = x * x * (3.0 - 2.0 * x);
    let own = 1.0 / (1.0 + (SUBS - 1) as f64 * g * g).sqrt();
    (depth, own, g * own)
}

/// A sine of phase `p` waves (0..2), to about 4e-6.
pub(super) fn sine(p: f64) -> f64 {
    // No `floor`: without SSE4.1 it is a library call, and this runs for every voice.
    let t = if p >= 1.0 { p - 1.0 } else { p };
    let y = if t < 0.25 {
        t
    } else if t < 0.75 {
        0.5 - t
    } else {
        t - 1.0
    };
    let x = TAU * y;
    let x2 = x * x;
    x * (1.0 - x2 / 6.0 * (1.0 - x2 / 20.0 * (1.0 - x2 / 42.0 * (1.0 - x2 / 72.0))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_approximations_hold() {
        for i in 0..=1000 {
            let p = i as f64 / 1000.0 * 2.0;
            assert!((sine(p) - (TAU * p).sin()).abs() < 5e-6, "{p}");
        }
        let (d, own, extra) = mix(0.0);
        assert_eq!((d, own, extra), (0.0, 1.0, 0.0));
        let (d, own, extra) = mix(1.0);
        assert!((d - SWARM_OCTAVES).abs() < 1e-12 && (own - extra).abs() < 1e-12);
    }
}
