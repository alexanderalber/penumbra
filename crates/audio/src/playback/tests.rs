use super::*;
use crate::fft::{Complex, Fft};

const AMP: f64 = 0.5;

/// A deterministic random sequence in [0, 1).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// A show that cooks one block per frame of its clock (late by a random jitter, now and then a
/// frame skipped and its samples sent with the next, never two in a row), and a device that pulls fixed blocks on
/// its own crystal.
struct Sim {
    engine: Engine,
    src_rate: u32,
    dev_rate: u32,
    /// The device crystal's error (0.0002: 200 ppm fast).
    drift: f64,
    fps: f64,
    jitter: f64,
    skip_chance: f64,
    freq: f64,
    dev_block: usize,
    /// Frames of the show produced.
    frame: u64,
    /// Source samples produced (the sine's phase).
    produced: u64,
    /// Device blocks pulled.
    pulls: u64,
    rng: Lcg,
    /// Interleaved stereo, kept for one block.
    out: Vec<f32>,
    /// The show stops producing between these times.
    pause: Option<(f64, f64)>,
    /// The device stops pulling between these times.
    stall: Option<(f64, f64)>,
    pending: u64,
}

impl Sim {
    fn new(src_rate: u32, dev_rate: u32, drift: f64) -> Self {
        let mut engine = Engine::new(Settings::default());
        engine.set_device(dev_rate, 2);
        Self {
            engine,
            src_rate,
            dev_rate,
            drift,
            fps: 60.0,
            jitter: 0.008,
            skip_chance: 0.03,
            freq: 997.0,
            dev_block: 480,
            frame: 0,
            produced: 0,
            pulls: 0,
            rng: Lcg(7),
            out: vec![0.0; 480 * 2],
            pause: None,
            stall: None,
            pending: 0,
        }
    }

    fn sample(&self, n: u64) -> f64 {
        AMP * (std::f64::consts::TAU * self.freq * n as f64 / f64::from(self.src_rate)).sin()
    }

    /// Runs `seconds` of true time; `look` sees every pulled block (interleaved stereo) and the
    /// time at its start.
    fn run(&mut self, seconds: f64, mut look: impl FnMut(f64, &[f32], &EngineStatus)) {
        let dev_true = f64::from(self.dev_rate) * (1.0 + self.drift);
        let end = seconds;
        let mut next_cook = self.frame as f64 / self.fps + self.rng.next() * self.jitter;
        loop {
            let next_pull = self.pulls as f64 * self.dev_block as f64 / dev_true;
            if next_pull.min(next_cook) >= end {
                break;
            }
            if next_cook <= next_pull {
                let t = next_cook;
                // Samples for this show frame.
                let to = ((self.frame + 1) as f64 * f64::from(self.src_rate) / self.fps).floor() as u64;
                let block = to - self.produced - self.pending;
                self.frame += 1;
                let paused = self.pause.is_some_and(|(a, b)| t >= a && t < b);
                if paused {
                    // The show's clock goes on; the samples for these frames are never made.
                    self.produced += block;
                } else if self.pending == 0 && self.rng.next() < self.skip_chance {
                    // One frame skipped at a time; its samples come with the next.
                    self.pending += block;
                } else {
                    let n = (block + self.pending) as usize;
                    let start = self.produced;
                    let l: Vec<f32> = (0..n as u64).map(|i| self.sample(start + i) as f32).collect();
                    self.engine.push(self.src_rate, &[&l, &l]).unwrap();
                    self.produced += n as u64;
                    self.pending = 0;
                }
                next_cook = self.frame as f64 / self.fps + self.rng.next() * self.jitter;
            } else {
                let t = next_pull;
                self.pulls += 1;
                if self.stall.is_some_and(|(a, b)| t >= a && t < b) {
                    continue;
                }
                self.out.resize(self.dev_block * 2, 0.0);
                self.engine.pull(&mut self.out);
                let st = self.engine.status();
                look(t, &self.out, &st);
            }
        }
    }
}

/// The largest step between neighbouring samples a sine of `freq` at `rate` has.
fn max_step(freq: f64, rate: u32) -> f64 {
    AMP * 2.0 * (std::f64::consts::PI * freq / f64::from(rate)).sin()
}

/// Watches the left channel: biggest step, rising zero crossings (interpolated), sum of squares.
#[derive(Default)]
struct Probe {
    prev: Option<f32>,
    n: u64,
    max_step: f64,
    first_cross: Option<f64>,
    last_cross: f64,
    crosses: u64,
    sum_sq: f64,
    count: u64,
    zeros_run: u64,
    longest_zeros: u64,
}

impl Probe {
    fn feed(&mut self, block: &[f32], measure: bool) {
        for f in block.as_chunks::<2>().0.iter() {
            let y = f[0];
            assert!(y.is_finite() && y.abs() <= 1.0);
            assert_eq!(f[0], f[1], "both channels carry the same signal");
            if let Some(p) = self.prev {
                self.max_step = self.max_step.max(f64::from((y - p).abs()));
                if measure && p < 0.0 && y >= 0.0 {
                    let at = self.n as f64 - 1.0 + f64::from(p / (p - y));
                    self.first_cross.get_or_insert(at);
                    self.last_cross = at;
                    self.crosses += 1;
                }
            }
            if measure {
                self.sum_sq += f64::from(y) * f64::from(y);
                self.count += 1;
            }
            if y == 0.0 {
                self.zeros_run += 1;
                self.longest_zeros = self.longest_zeros.max(self.zeros_run);
            } else {
                self.zeros_run = 0;
            }
            self.prev = Some(y);
            self.n += 1;
        }
    }

    fn freq(&self, rate: u32) -> f64 {
        let span = self.last_cross - self.first_cross.unwrap_or(0.0);
        (self.crosses - 1) as f64 / span * f64::from(rate)
    }

    fn rms(&self) -> f64 {
        (self.sum_sq / self.count.max(1) as f64).sqrt()
    }
}

fn steady(src: u32, dev: u32, drift: f64) {
    let seconds = 300.0;
    let settled = 150.0;
    let mut corr_sum = 0.0;
    let mut corr_n = 0.0;
    let mut sim = Sim::new(src, dev, drift);
    let mut probe = Probe::default();
    let (mut lo, mut hi) = (f64::MAX, f64::MIN);
    let (mut clo, mut chi) = (f64::MAX, f64::MIN);
    let target = Settings::default().latency_seconds() * 1000.0;
    sim.run(seconds, |t, block, st| {
        probe.feed(block, t > settled);
        if t > settled {
            corr_sum += st.correction_ppm;
            corr_n += 1.0;
        }
        if t > 60.0 {
            lo = lo.min(st.fill_ms);
            hi = hi.max(st.fill_ms);
            clo = clo.min(st.correction_ppm);
            chi = chi.max(st.correction_ppm);
            assert_eq!(st.state, State::Playing);
        }
    });
    let st = sim.engine.status();
    let what = format!(
        "{src} -> {dev} Hz, drift {:+.0} ppm, correction {clo:.0}..{chi:.0} ppm, fill {lo:.2}..{hi:.2} ms: {st:?}",
        drift * 1e6
    );
    println!("{what}");
    assert_eq!((st.underruns, st.overruns), (0, 0), "{what}");
    // The smoothed fill stays at the target once the loop has settled.
    assert!(lo > target - 2.0 && hi < target + 2.0, "fill {lo:.2}..{hi:.2} ms; {what}");
    // The instantaneous fill swings by a show frame and a device block around it.
    assert!(st.fill_now_ms > target - 30.0 && st.fill_now_ms < target + 30.0, "{what}");
    // The loop has learnt the crystal: it consumes the source slower when the device is fast.
    let mean = corr_sum / corr_n;
    assert!((mean + drift * 1e6).abs() < 10.0, "mean correction {mean:.1} ppm; {what}");
    assert!(chi - clo < 200.0, "{what}");
    // The tone in true time: measured on the device's nominal rate, corrected by its error.
    let f = probe.freq(dev) * (1.0 + drift);
    assert!((f - sim.freq).abs() < 0.005, "{f} Hz; {what}");
    let rms = probe.rms();
    assert!((rms - AMP / 2f64.sqrt()).abs() < 0.005, "rms {rms}; {what}");
    // No jumps beyond what the sine itself has (the fade at the start included).
    let limit = max_step(sim.freq, dev) * 1.02 + 1e-4;
    assert!(probe.max_step < limit, "step {} > {limit}; {what}", probe.max_step);
}

#[test]
fn same_rate_follows_a_fast_and_a_slow_crystal() {
    steady(48_000, 48_000, 200e-6);
    steady(48_000, 48_000, -200e-6);
}

#[test]
fn a_44_1_khz_device_plays_a_48_khz_source() {
    steady(48_000, 44_100, 200e-6);
    steady(48_000, 44_100, -200e-6);
}

#[test]
fn a_44_1_khz_source_plays_on_a_48_khz_device() {
    steady(44_100, 48_000, 150e-6);
}

#[test]
fn resampled_output_has_no_spurs() {
    // A 48 kHz tone on a 44.1 kHz device: all energy at the tone, nothing more than 200 Hz away
    // from it above -60 dB.
    let mut sim = Sim::new(48_000, 44_100, 0.0);
    sim.freq = 5_000.0;
    let mut tail: Vec<f32> = Vec::new();
    sim.run(4.0, |t, block, _| {
        if t > 3.0 {
            tail.extend(block.as_chunks::<2>().0.iter().map(|f| f[0]));
        }
    });
    let n = 8192;
    let fft = Fft::new(n).unwrap();
    let mut data: Vec<Complex> = tail[..n]
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let w = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos();
            Complex { re: v * w as f32, im: 0.0 }
        })
        .collect();
    fft.forward(&mut data);
    let mags: Vec<f32> = data[..n / 2].iter().map(|c| c.norm()).collect();
    let (peak_bin, peak) =
        mags.iter().enumerate().fold((0, 0.0f32), |a, (i, &m)| if m > a.1 { (i, m) } else { a });
    let peak_hz = peak_bin as f64 * 44_100.0 / n as f64;
    assert!((peak_hz - 5_000.0).abs() < 10.0, "{peak_hz}");
    let (spur_bin, spur) = mags
        .iter()
        .enumerate()
        // Aliases and images, not the skirt the slowly moving ratio puts around the tone.
        .filter(|(i, _)| (*i as i64 - peak_bin as i64).abs() > 40)
        .fold((0, 0.0f32), |a, (i, &m)| if m > a.1 { (i, m) } else { a });
    println!("spur at {:.0} Hz", spur_bin as f64 * 44_100.0 / n as f64);
    let db = 20.0 * (spur / peak).log10();
    println!("largest spur {db:.1} dB");
    assert!(db < -60.0, "largest spur {db:.1} dB");
}

#[test]
fn a_late_cook_fades_to_silence_and_recovers() {
    let mut sim = Sim::new(48_000, 48_000, 100e-6);
    sim.pause = Some((5.0, 5.2));
    let mut probe = Probe::default();
    let mut after = Probe::default();
    sim.run(8.0, |t, block, _| {
        probe.feed(block, false);
        if t > 7.0 {
            after.feed(block, true);
        }
    });
    let st = sim.engine.status();
    assert_eq!((st.underruns, st.overruns), (1, 0), "{st:?}");
    // Silence for the gap (less the fill that was queued), then the target again before it
    // plays.
    assert!(probe.longest_zeros > 48_000 / 10, "{}", probe.longest_zeros);
    assert!(probe.max_step < max_step(sim.freq, 48_000) * 1.05, "{}", probe.max_step);
    assert!((after.rms() - AMP / 2f64.sqrt()).abs() < 0.005, "{}", after.rms());
    assert_eq!(st.state, State::Playing);
    assert!((st.fill_ms - 50.0).abs() < 15.0, "{st:?}");
}

#[test]
fn a_stalled_device_drops_the_oldest_and_recovers() {
    let mut sim = Sim::new(48_000, 48_000, 0.0);
    sim.stall = Some((5.0, 5.3));
    let mut probe = Probe::default();
    sim.run(9.0, |_, block, _| probe.feed(block, false));
    let st = sim.engine.status();
    assert_eq!((st.underruns, st.overruns), (0, 1), "{st:?}");
    assert!(st.dropped_frames > 48_000 / 4, "{st:?}");
    assert!(probe.max_step < max_step(sim.freq, 48_000) * 1.05, "{}", probe.max_step);
    assert_eq!(st.state, State::Playing);
    assert!((st.fill_ms - 50.0).abs() < 10.0, "{st:?}");
}

#[test]
fn a_big_block_after_a_stall_is_cut_to_the_target() {
    let mut e = Engine::new(Settings::default());
    e.set_device(48_000, 2);
    let tone: Vec<f32> = (0..48_000).map(|i| 0.3 * (i as f32 * 0.05).sin()).collect();
    let mut out = vec![0.0f32; 960];
    // Playing steadily with a target fill, then 400 ms arrive at once.
    e.push(48_000, &[&tone[..2400]]).unwrap();
    for _ in 0..20 {
        e.pull(&mut out);
        e.push(48_000, &[&tone[..480]]).unwrap();
    }
    assert_eq!(e.status().state, State::Playing);
    e.push(48_000, &[&tone[..19_200]]).unwrap();
    for _ in 0..40 {
        e.pull(&mut out);
        e.push(48_000, &[&tone[..480]]).unwrap();
    }
    let st = e.status();
    assert_eq!((st.overruns, st.underruns), (1, 0), "{st:?}");
    assert!(st.fill_now_ms < 60.0, "{st:?}");
}

#[test]
fn the_first_samples_wait_for_the_target_and_fade_in() {
    let mut e = Engine::new(Settings { latency_ms: 20.0, ..Settings::default() });
    e.set_device(48_000, 1);
    let ones = vec![0.5f32; 480];
    let mut out = vec![0.0f32; 480];
    e.push(48_000, &[&ones]).unwrap();
    e.pull(&mut out);
    assert!(out.iter().all(|v| *v == 0.0), "10 ms queued, 20 wanted: silence");
    e.push(48_000, &[&ones]).unwrap();
    e.push(48_000, &[&ones]).unwrap();
    e.pull(&mut out);
    assert_eq!(out[0], 0.0);
    // The fade takes 4 ms (192 samples) and rises steadily.
    assert!(out.windows(2).take(190).all(|w| w[1] >= w[0]));
    assert!((out[300] - 0.5).abs() < 1e-3, "{}", out[300]);
}

#[test]
fn channels_map_onto_the_device() {
    let one = vec![0.25f32; 4800];
    let two = vec![-0.25f32; 4800];
    let three = vec![0.5f32; 4800];
    let settle = |e: &mut Engine, out: &mut Vec<f32>| {
        for _ in 0..5 {
            e.pull(out);
        }
    };
    // Mono onto four channels: all of them.
    let mut e = Engine::default();
    e.set_device(48_000, 4);
    e.push(48_000, &[&one]).unwrap();
    let mut out = vec![0.0f32; 400];
    settle(&mut e, &mut out);
    assert!(
        out.as_chunks::<4>().0.iter().all(|f| f.iter().all(|v| (v - 0.25).abs() < 1e-4)),
        "{:?}",
        &out[..8]
    );
    // Stereo onto four: the first two, the rest silent.
    let mut e = Engine::default();
    e.set_device(48_000, 4);
    e.push(48_000, &[&one, &two]).unwrap();
    settle(&mut e, &mut out);
    let f = &out[396..];
    assert!((f[0] - 0.25).abs() < 1e-4 && (f[1] + 0.25).abs() < 1e-4 && f[2] == 0.0 && f[3] == 0.0, "{f:?}");
    // Three onto two: by index.
    let mut e = Engine::default();
    e.set_device(48_000, 2);
    e.push(48_000, &[&one, &two, &three]).unwrap();
    settle(&mut e, &mut out);
    let f = &out[398..];
    assert!((f[0] - 0.25).abs() < 1e-4 && (f[1] + 0.25).abs() < 1e-4, "{f:?}");
    // Stereo onto mono: the mean.
    let mut e = Engine::default();
    e.set_device(48_000, 1);
    e.push(48_000, &[&one, &three]).unwrap();
    settle(&mut e, &mut out);
    assert!((out[399] - 0.375).abs() < 1e-4, "{}", out[399]);
    // f64 input works the same.
    let mut e = Engine::default();
    e.set_device(48_000, 1);
    let wide = vec![0.125f64; 4800];
    e.push(48_000, &[&wide]).unwrap();
    settle(&mut e, &mut out);
    assert!((out[399] - 0.125).abs() < 1e-4);
}

#[test]
fn hostile_input_is_refused_or_silenced() {
    let mut e = Engine::default();
    let empty: [&[f32]; 0] = [];
    assert!(e.push(48_000, &empty).is_err());
    assert!(e.push(0, &[&[0.0f32][..]]).is_err());
    assert!(e.push(u32::MAX, &[&[0.0f32][..]]).is_err());
    assert!(e.push(500, &[&[0.0f32][..]]).is_err());
    assert!(e.push(48_000, &[&[0.0f32, 1.0][..], &[0.0f32][..]]).is_err());
    let many: Vec<&[f32]> = vec![&[0.0f32][..]; MAX_CHANNELS + 1];
    assert!(e.push(48_000, &many).is_err());
    assert!(e.push_interleaved(48_000, 0, &[1.0]).is_err());
    assert_eq!(e.push_interleaved(48_000, 2, &[1.0, 1.0, 1.0]).unwrap(), 1);
    // No device: silence, nothing breaks.
    let mut out = vec![1.0f32; 64];
    e.pull(&mut out);
    assert!(out.iter().all(|v| *v == 0.0));
    // A device with odd shapes.
    e.set_device(0, 0);
    e.pull(&mut out);
    e.set_device(48_000, 3);
    let mut odd = vec![0.0f32; 7];
    e.pull(&mut odd);
    e.pull(&mut []);
    // Non-numbers and huge values: finite, within full scale.
    let bad: Vec<f32> = (0..9600)
        .map(|i| match i % 4 {
            0 => f32::NAN,
            1 => f32::INFINITY,
            2 => -1e30,
            _ => 1e30,
        })
        .collect();
    e.push(48_000, &[&bad]).unwrap();
    let bad64 = vec![f64::MAX; 4800];
    e.push(48_000, &[&bad64]).unwrap();
    let mut out = vec![0.0f32; 3 * 4800];
    for _ in 0..4 {
        e.pull(&mut out);
        assert!(out.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
    }
    // Extreme rate pairs: no panic, finite output.
    for (src, dev) in [(MIN_RATE, 768_000), (MAX_RATE, 8_000), (MAX_RATE, 1)] {
        let mut e = Engine::new(Settings { latency_ms: f64::NAN, ..Settings::default() });
        e.set_device(dev, 2);
        let block = vec![0.5f32; 20_000];
        let mut out = vec![0.0f32; 2048];
        for _ in 0..20 {
            e.push(src, &[&block]).unwrap();
            e.pull(&mut out);
            assert!(out.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
        }
    }
    // A block far larger than the ring keeps its newest part.
    let mut e = Engine::default();
    e.set_device(48_000, 1);
    let huge = vec![0.1f32; 48_000 * 10];
    e.push(48_000, &[&huge]).unwrap();
    let st = e.status();
    assert!(st.dropped_frames > 0 && st.fill_now_ms < 2000.0, "{st:?}");
    // Odd settings.
    for ms in [f64::NAN, -5.0, 0.0, 1e12, f64::INFINITY] {
        e.set_settings(Settings { latency_ms: ms, ..Settings::default() });
        e.push(48_000, &[&huge[..4800]]).unwrap();
        let mut out = vec![0.0f32; 480];
        e.pull(&mut out);
    }
}

#[test]
fn a_format_change_starts_over() {
    let mut e = Engine::default();
    e.set_device(48_000, 2);
    let a = vec![0.2f32; 4800];
    e.push(48_000, &[&a]).unwrap();
    let mut out = vec![0.0f32; 960];
    e.pull(&mut out);
    assert_eq!(e.status().state, State::Playing);
    e.push(44_100, &[&a, &a]).unwrap();
    let st = e.status();
    assert_eq!((st.state, st.source_rate, st.source_channels), (State::Silent, 44_100, 2));
    e.push(44_100, &[&a, &a]).unwrap();
    e.pull(&mut out);
    e.pull(&mut out);
    assert!((out[958] - 0.2).abs() < 1e-3, "{}", out[958]);
}

#[test]
fn a_player_without_a_device_never_blocks_the_producer() {
    let p = Player::open("no such device 7f3a", Settings::default());
    let block = vec![0.0f32; 800];
    for _ in 0..200 {
        p.push(48_000, &[&block, &block]).unwrap();
    }
    let st = p.status();
    assert_eq!(st.key, "no such device 7f3a");
    assert_eq!(st.engine.pushed_frames, 160_000);
    p.close();
}

/// Plays 440 Hz at -20 dBFS for a second on the default output and prints the status.
#[test]
#[ignore = "plays a quiet tone on this machine's default output"]
fn plays_a_quiet_tone_on_the_default_output() {
    for d in output_devices().unwrap() {
        println!("output: {d:?}");
    }
    let p = Player::open("", Settings::default());
    let rate = 48_000u32;
    let start = Instant::now();
    let mut produced = 0u64;
    let mut frame = 0u64;
    while start.elapsed() < Duration::from_millis(1500) {
        frame += 1;
        let due = (frame * u64::from(rate)) / 60;
        let tone: Vec<f32> = (produced..due)
            .map(|n| 0.1 * (std::f64::consts::TAU * 440.0 * n as f64 / f64::from(rate)).sin() as f32)
            .collect();
        produced = due;
        if start.elapsed() < Duration::from_secs(1) {
            p.push(rate, &[&tone]).unwrap();
        }
        let next = Duration::from_secs_f64(frame as f64 / 60.0);
        std::thread::sleep(next.saturating_sub(start.elapsed()));
        if frame.is_multiple_of(15) {
            let st = p.status();
            println!(
                "{:>5} ms: open {} {:?} {} Hz {} ch {:?}, fill {:.1} ms, device {:.1} ms, latency {:.1} ms, \
                 state {:?}, underruns {}, overruns {}, ratio {:.6}, glitches {}",
                start.elapsed().as_millis(),
                st.open,
                st.device,
                st.rate,
                st.channels,
                st.format,
                st.engine.fill_ms,
                st.device_ms,
                st.latency_ms,
                st.engine.state,
                st.engine.underruns,
                st.engine.overruns,
                st.engine.ratio,
                st.glitches
            );
        }
    }
    let st = p.status();
    println!("final: {st:?}");
    p.close();
}

#[test]
fn a_pause_fades_out_without_an_underrun_and_starts_over() {
    let mut e = Engine::new(Settings::default());
    e.set_device(48_000, 1);
    let block = vec![0.5f32; 800];
    let mut out = vec![0.0f32; 480];
    // A second of steady play, push and pull at the same pace.
    let mut pulled = 0u64;
    for frame in 1..=60u64 {
        e.push(48_000, &[&block]).unwrap();
        while pulled * 60 < frame * 100 {
            e.pull(&mut out);
            pulled += 1;
        }
    }
    assert_eq!(e.status().state, State::Playing);
    e.pause();
    let mut last = Vec::new();
    for _ in 0..20 {
        e.pull(&mut out);
        last.push(out.iter().fold(0.0f32, |m, v| m.max(v.abs())));
    }
    // 4 ms fade: the first pull of 10 ms ends silent, the rest are silent.
    assert!(last[0] > 0.1, "{last:?}");
    assert!(last[1..].iter().all(|v| *v == 0.0), "{last:?}");
    let st = e.status();
    assert_eq!((st.underruns, st.paused, st.state), (0, true, State::Silent));
    assert!(st.fill_now_ms < 1.0, "what was queued is dropped: {}", st.fill_now_ms);
    // Pushing again waits for the target and fades in, as at the start.
    for frame in 1..=30u64 {
        e.push(48_000, &[&block]).unwrap();
        while pulled * 60 < (60 + frame) * 100 {
            e.pull(&mut out);
            pulled += 1;
        }
    }
    let st = e.status();
    assert_eq!((st.underruns, st.paused, st.state), (0, false, State::Playing));
}
