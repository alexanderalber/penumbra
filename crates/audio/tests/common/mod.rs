//! Synthetic signals for the analysis tests: a house loop with known beats, and material with
//! no beat (white and pink noise, something like speech).

// Each test crate uses a part of these.
#![allow(dead_code, clippy::indexing_slicing)]

/// A small deterministic noise source.
pub struct Rng(pub u64);

impl Rng {
    /// -1 to 1.
    pub fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }

    /// 0 to 1.
    pub fn unit(&mut self) -> f64 {
        f64::from(self.next()) * 0.5 + 0.5
    }
}

/// A house-like loop: a kick on every beat, a hi-hat between, a clap on beats 2 and 4, a bass
/// note per bar that changes on the downbeat. The tempo may change over time. Returns the
/// samples and the beat times.
pub fn house(rate: f64, seconds: f64, bpm: impl Fn(f64) -> f64, start: f64) -> (Vec<f32>, Vec<f64>) {
    let n = (rate * seconds) as usize;
    let mut out = vec![0.0f32; n];
    let mut beats = Vec::new();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut t = start;
    let mut k = 0usize;
    while t < seconds {
        beats.push(t);
        let period = 60.0 / bpm(t);
        let at = (t * rate) as usize;
        // Kick: a falling sine, 120 ms.
        for i in 0..(0.12 * rate) as usize {
            let s = i as f64 / rate;
            let f = 50.0 + 100.0 * (-s * 30.0).exp();
            let v = (std::f64::consts::TAU * f * s).sin() * (-s * 25.0).exp() * 0.8;
            if let Some(o) = out.get_mut(at + i) {
                *o += v as f32;
            }
        }
        // Hi-hat on the off-beat: noise, 50 ms, high-passed by differencing.
        let off = ((t + period / 2.0) * rate) as usize;
        let mut last = 0.0f32;
        for i in 0..(0.05 * rate) as usize {
            let s = i as f64 / rate;
            let x = rng.next();
            let v = (x - last) * 0.15 * (-s * 60.0).exp() as f32;
            last = x;
            if let Some(o) = out.get_mut(off + i) {
                *o += v;
            }
        }
        // Clap on 2 and 4.
        if k % 2 == 1 {
            for i in 0..(0.08 * rate) as usize {
                let s = i as f64 / rate;
                if let Some(o) = out.get_mut(at + i) {
                    *o += rng.next() * 0.25 * (-s * 40.0).exp() as f32;
                }
            }
        }
        // Bass: a note per bar, held.
        let note = [55.0, 55.0 * 1.335, 55.0 * 1.189, 55.0 * 1.498].get((k / 4) % 4).copied().unwrap_or(55.0);
        for i in (0.12 * rate) as usize..(period * rate) as usize {
            let s = (at + i) as f64 / rate;
            if let Some(o) = out.get_mut(at + i) {
                *o += ((std::f64::consts::TAU * note * s).sin() * 0.2) as f32;
            }
        }
        t += period;
        k += 1;
    }
    (out, beats)
}

/// White noise at `amplitude`.
pub fn white(rate: f64, seconds: f64, amplitude: f32, seed: u64) -> Vec<f32> {
    let mut rng = Rng(seed);
    (0..(rate * seconds) as usize).map(|_| rng.next() * amplitude).collect()
}

/// Pink noise (Paul Kellet's filter), peaks near `amplitude`.
pub fn pink(rate: f64, seconds: f64, amplitude: f32, seed: u64) -> Vec<f32> {
    let mut rng = Rng(seed);
    let mut b = [0.0f32; 7];
    (0..(rate * seconds) as usize)
        .map(|_| {
            let w = rng.next();
            b[0] = 0.99886 * b[0] + w * 0.0555179;
            b[1] = 0.99332 * b[1] + w * 0.0750759;
            b[2] = 0.96900 * b[2] + w * 0.153_852;
            b[3] = 0.86650 * b[3] + w * 0.3104856;
            b[4] = 0.55000 * b[4] + w * 0.5329522;
            b[5] = -0.7616 * b[5] - w * 0.0168980;
            let v = b[0] + b[1] + b[2] + b[3] + b[4] + b[5] + b[6] + w * 0.5362;
            b[6] = w * 0.115926;
            v * 0.11 * amplitude
        })
        .collect()
}

/// Something like speech: syllables of 80 to 300 ms with gaps of 30 to 250 ms and now and
/// then a pause, each a voiced buzz (100 to 180 Hz, gliding) through two resonances that move
/// per syllable, some with a hiss in front.
pub fn speech(rate: f64, seconds: f64, seed: u64) -> Vec<f32> {
    let n = (rate * seconds) as usize;
    let mut out = vec![0.0f32; n];
    let mut rng = Rng(seed);
    let mut t = 0.2;
    let mut glottis = 0.0f64;
    while t < seconds {
        let length = 0.08 + 0.22 * rng.unit();
        let pitch = 100.0 + 80.0 * rng.unit();
        let glide = 0.8 + 0.4 * rng.unit();
        let formants = [300.0 + 600.0 * rng.unit(), 900.0 + 1600.0 * rng.unit()];
        let hiss = rng.unit() < 0.3;
        let loud = 0.3 + 0.7 * rng.unit();
        // Two-pole resonators, one per formant.
        let mut state = [[0.0f64; 2]; 2];
        let coefs = formants.map(|f| {
            let r = (-std::f64::consts::PI * 120.0 / rate).exp();
            (2.0 * r * (std::f64::consts::TAU * f / rate).cos(), -r * r)
        });
        let start = (t * rate) as usize;
        let len = (length * rate) as usize;
        let lead = if hiss { (0.06 * rate) as usize } else { 0 };
        for i in 0..len + lead {
            let Some(o) = out.get_mut(start + i) else { break };
            if i < lead {
                let e = (std::f64::consts::PI * i as f64 / lead.max(1) as f64).sin();
                let x = rng.next();
                *o += x * 0.05 * e as f32;
                continue;
            }
            let j = i - lead;
            let s = j as f64 / len as f64;
            let f0 = pitch * (1.0 + (glide - 1.0) * s);
            glottis += f0 / rate;
            let pulse = if glottis >= 1.0 {
                glottis -= 1.0;
                1.0
            } else {
                0.0
            };
            let env = (std::f64::consts::PI * s).sin().powf(0.6) * loud;
            let mut y = 0.0;
            for (st, (a1, a2)) in state.iter_mut().zip(coefs) {
                let v = pulse + a1 * st[0] + a2 * st[1];
                st[1] = st[0];
                st[0] = v;
                y += v;
            }
            *o += (y * env * 0.02) as f32;
        }
        let gap = if rng.unit() < 0.12 { 0.4 + 0.6 * rng.unit() } else { 0.03 + 0.22 * rng.unit() };
        t += length + lead as f64 / rate + gap;
    }
    out
}
