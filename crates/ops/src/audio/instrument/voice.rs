//! The voices of `audio.instrument`: one sounding note each, synthesised sample by sample.
//!
//! Every preset is a small fixed recipe. Pluck, mallet and bell are sums of sine partials, each
//! a rotating phasor with its own exponential decay. The kick is a sine whose frequency falls
//! exponentially towards 45 Hz. The hat is white noise (a hash of the sample number, so
//! renders repeat) through a state-variable filter.
//!
//! A voice starts with a short linear attack, so a note never starts with a step. It ends when
//! its level has fallen 80 dB below its start, or after a fade when it is stolen. Nothing here
//! allocates.

use std::f64::consts::TAU;

use crate::audio::oscillator::noise;
use crate::audio::synth::{Svf, SvfCoef, TOP, fall, tau_of};

/// The presets, in the order of the `preset` choice.
pub const PRESETS: [&str; 5] = ["pluck", "mallet", "bell", "kick", "hat"];

/// A voice is done once its level is this far below its start (-80 dB).
const SILENT: f64 = 1e-4;
/// Most partials of one voice.
const PARTIALS: usize = 9;
/// Seconds a stolen voice takes to fade out.
pub const STEAL_SECONDS: f64 = 0.005;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Pluck,
    Mallet,
    Bell,
    Kick,
    Hat,
}

impl Preset {
    pub fn parse(name: &str) -> Self {
        match name {
            "mallet" => Self::Mallet,
            "bell" => Self::Bell,
            "kick" => Self::Kick,
            "hat" => Self::Hat,
            _ => Self::Pluck,
        }
    }

    /// Whether the note's pitch comes from key, scale and note.
    pub fn pitched(self) -> bool {
        !matches!(self, Self::Kick | Self::Hat)
    }
}

/// What a note is played with, read once at note-on.
#[derive(Clone, Copy, Debug)]
pub struct Play {
    pub preset: Preset,
    /// Hz; ignored by kick and hat.
    pub frequency: f64,
    /// 0..1.
    pub brightness: f64,
    /// Seconds, as the `decay` param.
    pub decay: f64,
    pub rate: f64,
}

/// A sine partial as a rotating phasor: `(re, im)` turns by `(c, s)` per sample, its amplitude
/// falls by `fall` per sample.
#[derive(Clone, Copy, Debug, Default)]
struct Partial {
    re: f64,
    im: f64,
    c: f64,
    s: f64,
    amp: f64,
    fall: f64,
}

impl Partial {
    fn new(frequency: f64, amp: f64, tau: f64, rate: f64) -> Self {
        if !(frequency > 0.0 && frequency < TOP * rate) {
            return Self::default();
        }
        let w = TAU * frequency / rate;
        Self { re: 1.0, im: 0.0, c: w.cos(), s: w.sin(), amp, fall: fall(tau, rate) }
    }
}

// Voices sit in a Vec kept across blocks; boxing the partials would allocate per note.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Copy, Debug)]
enum Body {
    /// Sine partials, each decaying on its own; `n` of them are used.
    Partials {
        p: [Partial; PARTIALS],
        n: usize,
        start: f64,
    },
    /// `df` is the frequency above the end of the sweep, falling by `sweep` per sample.
    Kick {
        phase: f64,
        df: f64,
        sweep: f64,
        amp: f64,
        fall: f64,
    },
    Hat {
        svf: Svf,
        coef: SvfCoef,
        amp: f64,
        fall: f64,
    },
}

/// The end of the kick's sweep, Hz.
const KICK_END: f64 = 45.0;
/// The time constant of the kick's sweep, seconds: about 150 ms from 300 Hz to 49 Hz.
const KICK_SWEEP_TAU: f64 = 0.035;

/// One sounding note.
#[derive(Clone, Copy, Debug)]
pub struct Voice {
    /// The sample it started at, counted from tick 0.
    pub start: i64,
    /// The lane that played it.
    pub lane: usize,
    pub preset: Preset,
    body: Body,
    /// The attack ramp, 0 to 1, rising by `attack` per sample.
    ramp: f64,
    attack: f64,
    /// The fade-out, 1 down to 0, falling by `fading` per sample (0: not fading).
    fade: f64,
    fading: f64,
    /// A fixed gain per preset, so presets sound about equally loud.
    gain: f64,
    /// The noise seed.
    seed: i64,
    done: bool,
}

impl Voice {
    /// A new voice for `play` starting at sample `start`; `None` when its pitch is out of reach
    /// (above 0.45 of the rate or below 8 Hz).
    pub fn new(play: &Play, start: i64, lane: usize) -> Option<Self> {
        let r = play.rate;
        let b = play.brightness.clamp(0.0, 1.0);
        let f = play.frequency;
        let decay = play.decay.clamp(0.001, 3600.0);
        if play.preset.pitched() && !(f >= 8.0 && f < TOP * r) {
            return None;
        }
        // The upper partials' share: 0.25 at brightness 0, 1 at 0.5, 1.75 at 1, per octave.
        let tilt = 0.25 + 1.5 * b;
        let partials = |table: &[(f64, f64, f64)], t20: f64| {
            let tau = tau_of(t20);
            let mut p = [Partial::default(); PARTIALS];
            let mut sum = 0.0;
            for (slot, &(ratio, amp, life)) in p.iter_mut().zip(table) {
                let a = amp * tilt.powf(ratio.log2().max(0.0));
                *slot = Partial::new(f * ratio, a, tau * life, r);
                sum += slot.amp;
            }
            // Peak at most 1 before the preset's gain.
            for slot in &mut p {
                slot.amp /= sum.max(1e-9);
            }
            Body::Partials { p, n: table.len().min(PARTIALS), start: 1.0 }
        };
        let (body, attack_s, gain) = match play.preset {
            // Partials 1, 2, 3 and 5 stand out; higher ones die sooner.
            Preset::Pluck => (
                partials(&[(1.0, 1.0, 1.0), (2.0, 0.5, 0.7), (3.0, 0.35, 0.5), (5.0, 0.2, 0.3)], decay),
                0.001,
                1.0,
            ),
            // A tuned bar: partials at 1, 4 and 10 times.
            Preset::Mallet => {
                (partials(&[(1.0, 1.0, 1.0), (4.0, 0.35, 0.3), (10.0, 0.12, 0.1)], decay * 1.2), 0.0007, 1.0)
            }
            // Hum, prime, tierce, quint, nominal and the inharmonic ones above, after the
            // partials of a church bell.
            Preset::Bell => (
                partials(
                    &[
                        (0.5, 0.5, 1.0),
                        (1.0, 1.0, 0.8),
                        (1.19, 0.45, 0.6),
                        (1.5, 0.3, 0.5),
                        (2.0, 0.5, 0.4),
                        (2.74, 0.25, 0.3),
                        (3.0, 0.2, 0.25),
                        (3.76, 0.15, 0.2),
                        (4.07, 0.12, 0.15),
                    ],
                    decay * 8.0,
                ),
                0.0015,
                1.0,
            ),
            Preset::Kick => {
                let df = (150.0 + 300.0 * b).min(TOP * r) - KICK_END;
                let body = Body::Kick {
                    phase: 0.0,
                    df,
                    sweep: fall(KICK_SWEEP_TAU, r),
                    amp: 1.0,
                    fall: fall(tau_of(decay), r),
                };
                (body, 0.0003, 0.9)
            }
            Preset::Hat => {
                let coef = SvfCoef::new(5000.0 + 5000.0 * b, 1.4, r);
                (
                    Body::Hat { svf: Svf::default(), coef, amp: 1.0, fall: fall(tau_of(decay * 0.2), r) },
                    0.0002,
                    0.6,
                )
            }
        };
        Some(Self {
            start,
            lane,
            preset: play.preset,
            body,
            ramp: 0.0,
            attack: 1.0 / (attack_s * r).max(1.0),
            fade: 1.0,
            fading: 0.0,
            gain,
            seed: start,
            done: false,
        })
    }

    /// Whether the voice has ended and can go.
    pub fn done(&self) -> bool {
        self.done
    }

    /// Whether the voice is fading out (stolen).
    pub fn fading(&self) -> bool {
        self.fading > 0.0
    }

    /// Fades the voice out over `seconds`; a voice already fading keeps the faster fade.
    pub fn fade_out(&mut self, seconds: f64, rate: f64) {
        let step = 1.0 / (seconds * rate).max(1.0);
        self.fading = self.fading.max(step);
    }

    /// Adds the voice's next `out.len()` samples to `out`; `at` is the sample number of
    /// `out[0]`.
    pub fn render(&mut self, out: &mut [f64], at: i64, r: f64) {
        if self.done {
            return;
        }
        for (i, o) in out.iter_mut().enumerate() {
            let x = match &mut self.body {
                Body::Partials { p, n, .. } => {
                    let mut x = 0.0;
                    for q in p.iter_mut().take(*n) {
                        x += q.amp * q.im;
                        let re = q.re * q.c - q.im * q.s;
                        q.im = q.re * q.s + q.im * q.c;
                        q.re = re;
                        q.amp *= q.fall;
                    }
                    x
                }
                Body::Kick { phase, df, sweep, amp, fall } => {
                    let x = *amp * (*phase * TAU).sin();
                    *phase = (*phase + (KICK_END + *df) / r).rem_euclid(1.0);
                    *df *= *sweep;
                    *amp *= *fall;
                    x
                }
                Body::Hat { svf, coef, amp, fall } => {
                    let (_, _, high) = svf.tick(noise(at + i as i64, self.seed), coef);
                    let x = *amp * high;
                    *amp *= *fall;
                    x
                }
            };
            *o += x * self.gain * self.ramp * self.fade;
            self.ramp = (self.ramp + self.attack).min(1.0);
            if self.fading > 0.0 {
                self.fade -= self.fading;
                if self.fade <= 0.0 {
                    self.done = true;
                    return;
                }
            }
        }
        self.done = self.done || self.silent();
        // The phasors drift from length 1 by rounding: set them back once per block.
        if let Body::Partials { p, .. } = &mut self.body {
            for q in p.iter_mut() {
                let m = (q.re * q.re + q.im * q.im).sqrt();
                if m > 0.0 {
                    q.re /= m;
                    q.im /= m;
                }
            }
        }
    }

    fn silent(&self) -> bool {
        match &self.body {
            Body::Partials { p, n, start } => p.iter().take(*n).all(|q| q.amp < SILENT * start),
            Body::Kick { amp, .. } | Body::Hat { amp, .. } => *amp < SILENT,
        }
    }
}
