//! The `rain` preset of `audio.ambience`: a soft bed of noise and many tiny droplets.
//!
//! The bed is white noise (the sample-number hash, so a render repeats) through three one-pole
//! low-passes, summed with a little of the noise itself: a pink-ish rush. Droplets start at
//! random samples, about 90 a second at `density` 0.5: each a sine of 1.5 to 5.5 kHz that dies
//! away within a few milliseconds (time constant about 1 ms), mostly quiet, now and then loud.
//! At most [`DROPS`] ring at once; a new one takes the place of the quietest. Everything then
//! passes a high-pass at 400 Hz and a low-pass that brightness moves (about 7.5 kHz at 0.5).
//!
//! `density` sets the droplets per second (3 at 0, 90 at 0.5, 600 at 1) and the bed's level
//! (faint at 0, so single drops stand out). `brightness` moves the low-pass and the droplets' pitch: 0 is dull, distant rain, 1
//! close and crisp.

use super::super::oscillator::noise as white;
use super::super::synth::{Svf, SvfCoef, TOP};
use super::swarm::sine;
use crate::chan::hash;

/// Droplets ringing at once at most.
const DROPS: usize = 16;
/// Seconds per droplet cell: at most one droplet starts in each.
const CELL_SECONDS: f64 = 0.0005;
/// A droplet's time constant, seconds (45 samples at 48 kHz).
const DROP_TAU_SECONDS: f64 = 45.0 / 48_000.0;
/// Below this a droplet has died away.
const QUIET: f64 = 1e-5;
/// The bed's three low-passes at 48 kHz: pole and gain.
const BED: [(f64, f64); 3] = [(0.997, 0.029_591), (0.985, 0.032_534), (0.95, 0.048_056)];
/// Samples between the updates of the brightness filter.
const CONTROL: i64 = 32;

/// One ringing droplet.
#[derive(Clone, Copy, Debug, Default)]
struct Droplet {
    /// Phase and step, in waves.
    phase: f64,
    dt: f64,
    /// The amplitude now; 0 is idle.
    amp: f64,
}

/// One lane's rain.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Rain {
    bed: [f64; 3],
    /// The bed's poles and gains, the droplets' decay per sample and the high-pass, for
    /// `rate` (`None`: not yet).
    rate: Option<u64>,
    poles: [(f64, f64); 3],
    decay: f64,
    hp_coef: SvfCoef,
    /// The low-pass's coefficients for the control segment `seg`.
    seg: Option<i64>,
    lp_coef: SvfCoef,
    hp: Svf,
    lp: Svf,
    drops: [Droplet; DROPS],
    /// The droplet cell seen last, and the sample a droplet starts on in it.
    cell: Option<i64>,
    onset: Option<i64>,
}

/// A hash as 0..1.
fn unit(h: u32) -> f64 {
    f64::from(h >> 8) / f64::from(1u32 << 24)
}

/// Droplets per second at `density` `d`: 3 at 0, 90 at 0.5, 600 at 1, evenly on a log scale
/// in each half.
pub(super) fn drops_per_second(d: f64) -> f64 {
    let d = d.clamp(0.0, 1.0);
    if d <= 0.5 { 3.0 * 30f64.powf(2.0 * d) } else { 90.0 * (600.0f64 / 90.0).powf(2.0 * d - 1.0) }
}

/// The bed's level at `density` `d`: faint under a drizzle, 1 at 0.5, louder in a downpour.
fn bed_level(d: f64) -> f64 {
    if d <= 0.5 { 0.15 + 1.7 * d } else { 1.0 + 1.2 * (d - 0.5) }
}

impl Rain {
    fn seed(lane: usize, what: i64) -> i64 {
        90_001 + 1013 * lane as i64 + what
    }

    fn setup(&mut self, rate: f64) {
        // The same filters at any rate: poles scaled to the rate, gains to the same DC gain.
        let scale = 48_000.0 / rate;
        for ((pole, gain), (p, g)) in self.poles.iter_mut().zip(BED) {
            *pole = p.powf(scale);
            *gain = g * (1.0 - *pole) / (1.0 - p);
        }
        self.decay = (-1.0 / (DROP_TAU_SECONDS * rate)).exp();
        self.hp_coef = SvfCoef::new(400.0, std::f64::consts::SQRT_2, rate);
        self.rate = Some(rate.to_bits());
        self.seg = None;
    }

    /// Starts a droplet if its cell has one at sample `t`; true if it did.
    fn droplets_at(&mut self, t: i64, b: f64, d: f64, lane: usize, rate: f64) -> bool {
        let len = (CELL_SECONDS * rate).round().max(1.0) as i64;
        let c = t.div_euclid(len);
        if self.cell != Some(c) {
            self.cell = Some(c);
            let h = hash(c, Self::seed(lane, 0));
            let chance = drops_per_second(d) * len as f64 / rate;
            self.onset = (f64::from(h & 0xFFFF) / 65_536.0 < chance)
                .then(|| c.saturating_mul(len) + i64::from(h >> 16) % len);
        }
        if self.onset != Some(t) {
            return false;
        }
        let u = unit(hash(c, Self::seed(lane, 1)));
        let v = unit(hash(c, Self::seed(lane, 2)));
        // 1.5 to 5.5 kHz at brightness 0.5, an octave lower to an octave higher across it.
        let hz = ((1500.0 + 4000.0 * u) * (b - 0.5).exp2()).min(TOP * rate);
        let quietest = self.drops.iter_mut().min_by(|a, b| a.amp.total_cmp(&b.amp));
        if let Some(slot) = quietest {
            // From phase 0: the sine starts at 0, no step.
            *slot = Droplet { phase: 0.0, dt: hz / rate, amp: 0.05 + 0.25 * v * v * v };
        }
        true
    }

    /// One sample at sample number `t` with brightness `b` and density `d`.
    pub(super) fn tick(&mut self, t: i64, b: f64, d: f64, lane: usize, rate: f64) -> f64 {
        if self.rate != Some(rate.to_bits()) {
            self.setup(rate);
        }
        let seg = t.div_euclid(CONTROL);
        if self.seg != Some(seg) {
            let cutoff = 7500.0 * (1.6 * (b - 0.5)).exp2();
            self.lp_coef = SvfCoef::new(cutoff, std::f64::consts::SQRT_2, rate);
            self.seg = Some(seg);
        }
        let w = white(t, Self::seed(lane, 3));
        let mut bed = 0.1 * w;
        for (y, (pole, gain)) in self.bed.iter_mut().zip(self.poles) {
            *y = pole * *y + gain * w;
            bed += *y;
        }
        self.droplets_at(t, b, d, lane, rate);
        let mut drops = 0.0;
        for dr in &mut self.drops {
            if dr.amp <= 0.0 {
                continue;
            }
            drops += dr.amp * sine(dr.phase);
            dr.phase += dr.dt;
            if dr.phase >= 1.0 {
                dr.phase -= 1.0;
            }
            dr.amp *= self.decay;
            if dr.amp < QUIET {
                dr.amp = 0.0;
            }
        }
        let x = 0.9 * bed * bed_level(d) + drops;
        let (_, _, high) = self.hp.tick(x, &self.hp_coef);
        let (low, _, _) = self.lp.tick(high, &self.lp_coef);
        low
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn density_maps_to_drops_per_second() {
        assert!((drops_per_second(0.0) - 3.0).abs() < 1e-9);
        assert!((drops_per_second(0.5) - 90.0).abs() < 1e-9);
        assert!((drops_per_second(1.0) - 600.0).abs() < 1e-9);
    }

    /// Over 20 s the droplets come at the rate the density asks for, at any sample rate.
    #[test]
    fn droplets_come_at_their_rate() {
        for rate in [8_000.0, 48_000.0, 192_000.0] {
            for d in [0.0, 0.5, 1.0] {
                let mut rain = Rain::default();
                let seconds = 20.0;
                let n = (seconds * rate) as i64;
                let started = (0..n).filter(|&t| rain.droplets_at(t, 0.5, d, 0, rate)).count();
                let expect = drops_per_second(d) * seconds;
                let off = (started as f64 - expect).abs() / expect;
                // Poisson: 3 per second gives 60 in 20 s, give or take 8.
                assert!(off < 0.3, "{rate} Hz, density {d}: {started} droplets, {expect} expected");
            }
        }
    }
}
