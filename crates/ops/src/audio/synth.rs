//! Small synthesis pieces shared by `audio.instrument` and `audio.ambience`: a state-variable
//! filter, the polyBLEP saw correction, decay and glide factors, and the soft ceiling every
//! output passes. Nothing here allocates.

use std::f64::consts::{LN_10, PI};

/// Partials above this share of the sample rate are left out (Nyquist is 0.5).
pub const TOP: f64 = 0.45;
/// Below this an output is passed as it is; above, it bends softly towards [`CEILING`], so a
/// pile of notes cannot reach full scale.
pub const KNEE: f64 = 0.5;
/// The most an output reaches (-0.2 dBFS).
pub const CEILING: f64 = 0.977;

/// A topology-preserving state-variable filter (Zavalishin), stable for every cutoff below
/// Nyquist.
#[derive(Clone, Copy, Debug, Default)]
pub struct Svf {
    ic1: f64,
    ic2: f64,
}

/// The coefficients of [`Svf`] for a cutoff and a damping `k` (1 / Q).
#[derive(Clone, Copy, Debug, Default)]
pub struct SvfCoef {
    k: f64,
    a1: f64,
    a2: f64,
    a3: f64,
}

impl SvfCoef {
    pub fn new(cutoff: f64, k: f64, rate: f64) -> Self {
        let g = (PI * cutoff.clamp(10.0, TOP * rate) / rate).tan();
        Self::of_g(g, k)
    }

    /// The coefficients for a prewarped cutoff `g` (`tan(pi * cutoff / rate)`) and a damping
    /// `k`, both above 0.
    pub fn of_g(g: f64, k: f64) -> Self {
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        Self { k, a1, a2, a3: g * a2 }
    }
}

impl Svf {
    /// One sample in; (low, band, high) out.
    pub fn tick(&mut self, x: f64, c: &SvfCoef) -> (f64, f64, f64) {
        let v3 = x - self.ic2;
        let v1 = c.a1 * self.ic1 + c.a2 * v3;
        let v2 = self.ic2 + c.a2 * self.ic1 + c.a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        (v2, v1, x - c.k * v1 - v2)
    }

    /// Whether the state holds only finite numbers.
    pub fn is_finite(&self) -> bool {
        self.ic1.is_finite() && self.ic2.is_finite()
    }
}

/// A band-limited saw (-1..1) at phase `t` (0..1) stepping `dt` per sample: the naive saw
/// minus the polyBLEP correction at its jump.
pub fn saw(t: f64, dt: f64) -> f64 {
    let blep = if t < dt {
        let x = t / dt;
        2.0 * x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + 2.0 * x + 1.0
    } else {
        0.0
    };
    2.0 * t - 1.0 - blep
}

/// The per-sample factor of an exponential decay with time constant `tau` seconds.
pub fn fall(tau: f64, rate: f64) -> f64 {
    (-1.0 / (tau.max(1e-4) * rate)).exp()
}

/// The time constant of a decay that falls by 20 dB (to a tenth) in `seconds`.
pub fn tau_of(seconds: f64) -> f64 {
    seconds / LN_10
}

/// The per-sample share of the way a one-pole glide moves that gets 90% there in `seconds`.
pub fn glide(seconds: f64, rate: f64) -> f64 {
    1.0 - (-LN_10 / (seconds.max(1e-3) * rate)).exp()
}

/// The gain of `level` dB; `min` and below is silence.
pub fn gain_of(level: f64, min: f64, max: f64) -> f64 {
    if !level.is_finite() || level <= min { 0.0 } else { 10f64.powf(level.min(max) / 20.0) }
}

/// Linear below [`KNEE`], then a soft bend towards [`CEILING`], with no kink at the knee.
pub fn soft_clip(x: f64) -> f64 {
    let a = x.abs();
    let room = CEILING - KNEE;
    if a <= KNEE { x } else { x.signum() * (KNEE + room * ((a - KNEE) / room).tanh()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soft_clip_is_transparent_below_the_knee_and_stays_below_the_ceiling() {
        assert_eq!(soft_clip(0.3), 0.3);
        assert_eq!(soft_clip(-0.5), -0.5);
        for x in [0.6, 1.0, 10.0, 1e9, -1e9] {
            let y = soft_clip(x);
            assert!(y.abs() <= CEILING && y.abs() > 0.5, "{x}: {y}");
        }
        assert_eq!(gain_of(-100.0, -100.0, 24.0), 0.0);
        assert!((gain_of(-6.0, -100.0, 24.0) - 0.501).abs() < 1e-3);
    }

    /// The band-limited saw has the naive saw's shape away from the jump and no DC.
    #[test]
    fn the_saw_is_a_saw() {
        let dt = 0.01;
        assert!((saw(0.5, dt) - 0.0).abs() < 1e-12);
        assert!((saw(0.25, dt) + 0.5).abs() < 1e-12);
        let mean = (0..100).map(|i| saw(i as f64 * dt, dt)).sum::<f64>() / 100.0;
        assert!(mean.abs() < 1e-3, "{mean}");
    }
}
