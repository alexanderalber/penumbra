//! An envelope follower: how loud a signal is now, sample by sample, with an attack and a
//! release.
//!
//! **Peak** follows the rectified signal in two stages. The first jumps up to every new crest
//! at once and falls exponentially with the release time. The second smooths that with the
//! attack time, rising and falling alike. A steady sine reads its amplitude: a sine at 0.1 reads
//! 0.1 within 0.01 dB at the default times. After a jump the reading gets 63 % of the way in
//! about the attack time; after the sound stops it falls to 37 % in about the release time
//! plus the attack time.
//!
//! **RMS** averages the squared signal with a one-pole whose time constant makes the reading
//! get 63 % of the way up in the attack time, and holds the root of that against a fall faster
//! than the release. A steady sine at amplitude 0.1 reads 0.0707 (-23 dB). Its fall is the
//! release time, unless that is shorter than about four times the attack time. Below about
//! 100 Hz the average keeps a ripple at short attack times, and the reading wobbles by a few
//! tenths of a dB.

/// How the level is measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Detect {
    /// The power averaged over about the attack time: loudness as heard.
    #[default]
    Rms,
    /// The crests of the waveform: what a peak meter shows.
    Peak,
}

/// Samples beyond this (60 dB above full scale) are clipped to it; non-finite ones count as 0.
pub const CLIP: f64 = 1000.0;
/// Below this a stage is set to 0, so silence does not decay through subnormal numbers.
const TINY: f64 = 1e-30;
/// The share of the averaging time constant after which the root of a rising average reaches
/// 63 %: `sqrt(1 - e^-t) = 1 - 1/e` at `t = -ln(1 - (1 - 1/e)^2)`.
const RMS_RISE: f64 = 0.510_2;

/// The one-pole coefficient for a time constant of `seconds` at `rate` samples per second: 0
/// follows at once, near 1 slowly.
pub fn coefficient(seconds: f64, rate: f64) -> f64 {
    if !(seconds.is_finite() && rate.is_finite()) || seconds <= 0.0 || rate <= 0.0 {
        return 0.0;
    }
    (-1.0 / (seconds * rate)).exp()
}

/// The follower's state for one channel.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Envelope {
    /// RMS: the averaged squared signal.
    power: f64,
    /// The first stage: up at once, down with the release.
    held: f64,
    /// Peak: the second stage, smoothed with the attack.
    level: f64,
}

impl Envelope {
    /// Follows `samples` at `rate` from this state; the level after the last sample, linear
    /// (full scale is 1).
    pub fn run(&mut self, samples: &[f64], rate: f64, attack: f64, release: f64, detect: Detect) -> f64 {
        let attack = if attack.is_finite() { attack.max(0.0) } else { 0.01 };
        let release = if release.is_finite() { release.max(0.0) } else { 0.2 };
        let fall = coefficient(release, rate);
        match detect {
            Detect::Peak => {
                let rise = coefficient(attack, rate);
                for &x in samples {
                    let d = if x.is_finite() { x.abs().min(CLIP) } else { 0.0 };
                    self.held = d.max(self.held * fall);
                    self.level = self.held + (self.level - self.held) * rise;
                }
            }
            Detect::Rms => {
                let rise = coefficient(attack / RMS_RISE, rate);
                for &x in samples {
                    let x = if x.is_finite() { x.clamp(-CLIP, CLIP) } else { 0.0 };
                    self.power = x * x + (self.power - x * x) * rise;
                    self.held = self.power.sqrt().max(self.held * fall);
                }
                self.level = self.held;
            }
        }
        for v in [&mut self.power, &mut self.held, &mut self.level] {
            if !v.is_finite() || *v < TINY {
                *v = 0.0;
            }
        }
        self.level
    }

    /// The level after the last [`Envelope::run`], linear.
    pub fn level(&self) -> f64 {
        self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(amp: f64, hz: f64, rate: f64, from: usize, n: usize) -> Vec<f64> {
        (from..from + n).map(|i| amp * (std::f64::consts::TAU * hz * i as f64 / rate).sin()).collect()
    }

    fn db(v: f64) -> f64 {
        20.0 * v.log10()
    }

    #[test]
    fn a_sine_reads_its_peak_and_its_rms() {
        let rate = 48_000.0;
        let x = sine(0.1, 1000.0, rate, 0, 48_000);
        let mut e = Envelope::default();
        let peak = e.run(&x, rate, 0.01, 0.2, Detect::Peak);
        assert!((db(peak) + 20.0).abs() < 0.2, "{}", db(peak));
        let mut e = Envelope::default();
        let rms = e.run(&x, rate, 0.01, 0.2, Detect::Rms);
        assert!((db(rms) + 23.01).abs() < 0.2, "{}", db(rms));
    }

    #[test]
    fn hostile_input_stays_finite() {
        for detect in [Detect::Peak, Detect::Rms] {
            let mut e = Envelope::default();
            let v = e.run(&[f64::NAN, f64::INFINITY, 1e30, -1e300, 0.5], 48_000.0, f64::NAN, -1.0, detect);
            assert!(v.is_finite() && v <= CLIP, "{v}");
            for rate in [1.0, 1e9, 0.0, f64::NAN] {
                let v = e.run(&[0.5; 64], rate, 0.01, 0.2, detect);
                assert!(v.is_finite(), "{rate}: {v}");
            }
        }
    }
}
