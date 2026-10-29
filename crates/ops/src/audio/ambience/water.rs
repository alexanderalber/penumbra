//! The `water` preset of `audio.ambience`: running water from white noise.
//!
//! One white noise (the sample-number hash, so a render repeats) feeds four bands: a low rumble
//! around 100 Hz, a body between 200 and 600 Hz, a splash around 4 kHz and a hiss above 6 kHz.
//! Each band's level and centre wander on their own, slowly and at random (gradient noise of
//! the sample number, periods from a fifth of a second to three seconds). Brightness weighs the
//! bands: 0 leans on the rumble and the body, 1 on the splash and the hiss. On top, small
//! resonant blips in the body's range rise quickly in pitch and die away within a few
//! milliseconds: bubbles. They come at random, more of them the brighter.
//!
//! Levels and centres change at control points [`CONTROL`] samples apart on the absolute sample
//! number. Between them the levels move in a straight line and the filters hold. Everything is
//! a function of the sample number and the state before it, so blocks join without a seam.

use std::f64::consts::{PI, TAU};

use super::super::oscillator::noise as white;
use super::super::synth::{Svf, SvfCoef, TOP};
use crate::chan::{hash, noise as smooth};

/// Samples between control points.
const CONTROL: i64 = 32;

#[derive(Clone, Copy, Debug)]
enum Pass {
    Band,
    High,
}

/// One band of the water.
struct Band {
    pass: Pass,
    /// The centre (band) or the corner (high), Hz.
    hz: f64,
    /// How far the centre wanders, octaves either side.
    roam: f64,
    /// Damping (1 / Q).
    k: f64,
    /// Seconds per wander of the level, and of the centre.
    level_period: f64,
    centre_period: f64,
    /// How far the level wanders, dB either side.
    depth_db: f64,
}

const BANDS: [Band; 4] = [
    // Rumble: 70 to 140 Hz, swelling over seconds.
    Band {
        pass: Pass::Band,
        hz: 100.0,
        roam: 0.5,
        k: 1.4,
        level_period: 2.6,
        centre_period: 3.1,
        depth_db: 6.0,
    },
    // Body: 200 to 600 Hz, the gurgle, about twice a second.
    Band {
        pass: Pass::Band,
        hz: 350.0,
        roam: 0.8,
        k: 0.8,
        level_period: 0.55,
        centre_period: 1.3,
        depth_db: 9.0,
    },
    // Splash: around 4 kHz, quick.
    Band {
        pass: Pass::Band,
        hz: 4000.0,
        roam: 0.5,
        k: 1.0,
        level_period: 0.3,
        centre_period: 0.9,
        depth_db: 9.0,
    },
    // Hiss: above 6 to 8 kHz, flickering.
    Band {
        pass: Pass::High,
        hz: 7000.0,
        roam: 0.25,
        k: 1.4,
        level_period: 0.17,
        centre_period: 1.7,
        depth_db: 6.0,
    },
];

/// Seconds per bubble cell: at most one bubble starts in each.
const CELL_SECONDS: f64 = 0.01;
/// Seconds a bubble rings before it is left alone (its ring is below -80 dB by then).
const RING_SECONDS: f64 = 0.08;
/// Seconds of the smooth push that starts a bubble.
const PUSH_SECONDS: f64 = 0.0002;
/// Octaves a bubble's pitch rises per second while it rings.
const RISE_OCTAVES_PER_SECOND: f64 = 20.0;
/// Samples between the updates of a rising bubble's filter.
const BUBBLE_STEP: i64 = 16;
/// The bubbles' level against the bands.
const BUBBLE_GAIN: f64 = 1.5;

/// A resonant blip.
#[derive(Clone, Copy, Debug, Default)]
struct Bubble {
    svf: Svf,
    coef: SvfCoef,
    /// The pitch, Hz, and the ring's time constant, seconds.
    hz: f64,
    tau: f64,
    /// Samples since it started; `None`: idle.
    age: Option<i64>,
    /// The push's strength.
    amp: f64,
}

impl Bubble {
    fn retune(&mut self, rate: f64) {
        self.hz = self.hz.clamp(20.0, TOP * rate);
        let k = (2.0 / (TAU * self.hz * self.tau)).clamp(0.005, 1.0);
        self.coef = SvfCoef::new(self.hz, k, rate);
    }

    /// One sample of the bubble, normalised so its ring peaks near `amp`.
    fn tick(&mut self, rate: f64) -> f64 {
        let Some(age) = self.age else { return 0.0 };
        if age as f64 > RING_SECONDS * rate {
            self.age = None;
            return 0.0;
        }
        let push_len = (PUSH_SECONDS * rate).max(2.0);
        let x = if (age as f64) < push_len {
            // A raised cosine: no step where the push starts or ends.
            self.amp * (1.0 - (TAU * age as f64 / push_len).cos())
        } else {
            0.0
        };
        if age > 0 && age % BUBBLE_STEP == 0 {
            self.hz *= (RISE_OCTAVES_PER_SECOND * BUBBLE_STEP as f64 / rate).exp2();
            self.retune(rate);
        }
        self.age = Some(age + 1);
        let (_, band, _) = self.svf.tick(x, &self.coef);
        // The push is short against the period, so the ring's height is about its area times
        // the angular frequency: undo that, so every pitch rings about as high.
        band * rate / (push_len * TAU * self.hz.max(1.0))
    }
}

/// One band's filter and its control values for the current segment.
#[derive(Clone, Copy, Debug, Default)]
struct Filter {
    svf: Svf,
    coef: SvfCoef,
    /// The gain (wander times make-up) at the segment's start, and its change per sample.
    gain: f64,
    step: f64,
}

/// One lane's water.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Water {
    bands: [Filter; 4],
    /// The control segment the bands' values belong to, and the rate they were made for.
    seg: Option<(i64, u64)>,
    bubbles: [Bubble; 2],
    /// The bubble cell seen last, and the sample a bubble starts on in it.
    cell: Option<i64>,
    onset: Option<i64>,
}

/// Smooth noise in about -1..1 with no regular returns to 0: two gradient noises at unrelated
/// rates.
fn wander(t: f64, seed: i64) -> f64 {
    let a = smooth(t, seed);
    let b = smooth(t * 2.71 + 0.37, seed.wrapping_add(1));
    ((a + 0.5 * b) / 1.2).clamp(-1.0, 1.0)
}

/// A hash as 0..1.
fn unit(h: u32) -> f64 {
    f64::from(h >> 8) / f64::from(1u32 << 24)
}

impl Water {
    fn seed(lane: usize, band: usize, what: i64) -> i64 {
        70_001 + 1009 * lane as i64 + 37 * band as i64 + what
    }

    /// Each band's gain at sample `t`: the level wander times the make-up that brings its share
    /// of white noise to the same loudness.
    fn gains(t: i64, lane: usize, rate: f64) -> [f64; 4] {
        let secs = t as f64 / rate;
        let mut g = [0.0; 4];
        for (j, (b, g)) in BANDS.iter().zip(&mut g).enumerate() {
            let n = wander(secs / b.level_period, Self::seed(lane, j, 0));
            *g = (b.depth_db / 20.0 * n * std::f64::consts::LOG2_10).exp2();
        }
        g
    }

    fn centre(t: i64, b: &Band, j: usize, lane: usize, rate: f64) -> f64 {
        let n = wander(t as f64 / rate / b.centre_period, Self::seed(lane, j, 1));
        (b.hz * (b.roam * n).exp2()).clamp(10.0, TOP * rate)
    }

    /// Makes the control values for the segment `seg`.
    fn control(&mut self, seg: i64, lane: usize, rate: f64) {
        let t0 = seg.saturating_mul(CONTROL);
        let start = Self::gains(t0, lane, rate);
        let end = Self::gains(t0.saturating_add(CONTROL), lane, rate);
        let nyquist = rate / 2.0;
        for (j, ((b, f), (start, end))) in
            BANDS.iter().zip(&mut self.bands).zip(start.iter().zip(end)).enumerate()
        {
            let hz = Self::centre(t0, b, j, lane, rate);
            f.coef = SvfCoef::new(hz, b.k, rate);
            // Make-up: white noise of RMS r through the normalised band has RMS
            // r * sqrt(pi k hz / rate); through the high-pass about r * sqrt(share above hz).
            let makeup = match b.pass {
                Pass::Band => (rate / (PI * b.k * hz)).sqrt(),
                Pass::High => (nyquist / (nyquist - hz).max(1.0)).sqrt(),
            }
            .min(30.0);
            f.gain = start * makeup;
            f.step = (end - start) * makeup / CONTROL as f64;
        }
        self.seg = Some((seg, rate.to_bits()));
    }

    /// The bands' weights at brightness `b`, scaled so their sum keeps about one loudness.
    fn weights(b: f64) -> [f64; 4] {
        let w = [0.15 + (1.0 - b) * (1.0 - b), 0.75 - 0.35 * b, 0.12 + 0.7 * b, 0.04 + 0.6 * b * b];
        let norm = 1.0 / w.iter().map(|x| x * x).sum::<f64>().sqrt();
        w.map(|x| x * norm)
    }

    /// Starts a bubble if cell `c` has one at sample `t`.
    fn bubbles_at(&mut self, t: i64, b: f64, lane: usize, rate: f64) {
        let len = (CELL_SECONDS * rate).round().max(1.0) as i64;
        let c = t.div_euclid(len);
        if self.cell != Some(c) {
            self.cell = Some(c);
            let h = hash(c, Self::seed(lane, 9, 0));
            // About 6 bubbles a second at brightness 0, 26 at 1.
            let chance = 0.06 + 0.2 * b;
            self.onset = (f64::from(h & 0xFFFF) / 65_536.0 < chance)
                .then(|| c.saturating_mul(len) + i64::from(h >> 16) % len);
        }
        if self.onset != Some(t) {
            return;
        }
        let u = unit(hash(c, Self::seed(lane, 9, 1)));
        let v = unit(hash(c, Self::seed(lane, 9, 2)));
        let w = unit(hash(c, Self::seed(lane, 9, 3)));
        let [even, odd] = &mut self.bubbles;
        let slot = if c.rem_euclid(2) == 1 { odd } else { even };
        // 300 Hz to 1.2 kHz, a little higher when bright; rings for 3 to 8 ms.
        slot.hz = 300.0 * (2.0 * u + 0.6 * b).exp2();
        slot.tau = 0.003 + 0.005 * v;
        slot.amp = 0.3 + 0.7 * w;
        slot.age = Some(0);
        slot.retune(rate);
    }

    /// One sample at sample number `t` with brightness `b`, about RMS 0.6 before the wander.
    pub(super) fn tick(&mut self, t: i64, b: f64, lane: usize, rate: f64) -> f64 {
        let seg = t.div_euclid(CONTROL);
        if self.seg != Some((seg, rate.to_bits())) {
            self.control(seg, lane, rate);
        }
        let j = (t - seg.saturating_mul(CONTROL)) as f64;
        let x = white(t, Self::seed(lane, 8, 0));
        let w = Self::weights(b);
        let mut y = 0.0;
        for ((band, f), w) in BANDS.iter().zip(&mut self.bands).zip(w) {
            let (_, bp, hp) = f.svf.tick(x, &f.coef);
            let shaped = match band.pass {
                Pass::Band => band.k * bp,
                Pass::High => hp,
            };
            y += shaped * w * (f.gain + f.step * j);
        }
        self.bubbles_at(t, b, lane, rate);
        let bubbles: f64 = self.bubbles.iter_mut().map(|s| s.tick(rate)).sum();
        y + BUBBLE_GAIN * bubbles
    }
}
