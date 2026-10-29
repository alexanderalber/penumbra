//! `audio.filter`: the response of every type at several rates and cutoffs, stability under
//! fast modulation, no zipper on a sweep, the block contract (a recook, a device that
//! delivered nothing, channels by name), hostile params and inputs; and ignored tests that
//! write a listening file and measure the cost.

use penumbra_audio::Hub;
use penumbra_core::{Channel, Chans, Name, ParamPath};
use penumbra_graph::{Data, Op, Port};

use super::filter::{FILTER, Kind, Settings, State, design, q_of};
use super::instrument_tests::{LISTEN_DIR, db, peak, run, write_wav};
use super::oscillator::noise;
use super::tests::{F, connect, create, set};

const RATES: [f64; 3] = [44_100.0, 48_000.0, 96_000.0];

fn settings(kind: Kind, cutoff: f64) -> Settings {
    Settings { kind, steep: false, cutoff, resonance: 0.2, gain: 0.0, mix: 1.0 }
}

fn chans(rate: f64, channels: &[(&str, Vec<f64>)]) -> Chans {
    Chans::new(
        rate,
        channels.iter().map(|(n, s)| Channel { name: Name::new(n).unwrap(), samples: s.clone() }).collect(),
    )
    .unwrap()
}

fn sine(hz: f64, rate: f64, n: usize) -> Vec<f64> {
    (0..n).map(|i| (std::f64::consts::TAU * hz * i as f64 / rate).sin()).collect()
}

fn rms(x: &[f64]) -> f64 {
    (x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64).sqrt()
}

/// The gain in dB of the filter at `hz`: a sine for 0.4 s, measured over its last 0.2 s.
fn response(s: &Settings, rate: f64, hz: f64) -> f64 {
    let n = (rate * 0.4) as usize;
    let out = State::default().run(&chans(rate, &[("ch1", sine(hz, rate, n))]), &design(s, rate)).unwrap();
    let y = &out.channels()[0].samples[n / 2..];
    db(rms(y) / std::f64::consts::FRAC_1_SQRT_2)
}

fn near(what: &str, got: f64, want: f64, tol: f64) {
    eprintln!("{what}: {got:+.2} dB (want {want:+.1})");
    assert!((got - want).abs() <= tol, "{what}: {got:.2} dB, want {want} +- {tol}");
}

#[test]
fn resonance_maps_to_q() {
    assert!((q_of(0.0) - 0.5).abs() < 1e-12);
    assert!((q_of(0.2) - 0.695).abs() < 0.01);
    assert!((q_of(1.0) - 20.0).abs() < 1e-9);
    assert!((q_of(f64::NAN) - q_of(0.2)).abs() < 1e-12);
}

/// Lowpass: -3 dB at cutoff, 12 or 24 dB per octave beyond; highpass mirrored.
#[test]
fn lowpass_and_highpass_fall_off_at_their_slopes() {
    for rate in RATES {
        for cutoff in [200.0, 1000.0, 4000.0] {
            for steep in [false, true] {
                let per_octave = if steep { 24.0 } else { 12.0 };
                let lp = Settings { steep, ..settings(Kind::Lowpass, cutoff) };
                let hp = Settings { steep, ..settings(Kind::Highpass, cutoff) };
                let tag = |k: &str| format!("{k} {per_octave} dB/oct, {cutoff} Hz at {rate}");
                near(&format!("{} at cutoff", tag("lowpass")), response(&lp, rate, cutoff), -3.0, 0.6);
                near(
                    &format!("{} at cutoff / 8", tag("lowpass")),
                    response(&lp, rate, cutoff / 8.0),
                    0.0,
                    0.3,
                );
                // Three octaves above, where the slope has set in; the bilinear transform
                // only steepens it towards Nyquist.
                if cutoff * 8.0 < 0.3 * rate {
                    let got = response(&lp, rate, cutoff * 8.0);
                    let want = -3.0 * per_octave;
                    eprintln!("{} at cutoff * 8: {got:+.2} dB (want {want:+.1} or steeper)", tag("lowpass"));
                    assert!(got < want + 1.0 && got > want - 8.0, "{got}");
                }
                near(&format!("{} at cutoff", tag("highpass")), response(&hp, rate, cutoff), -3.0, 0.6);
                if cutoff * 8.0 < 0.3 * rate {
                    let got = response(&hp, rate, cutoff * 8.0);
                    near(&format!("{} at cutoff * 8", tag("highpass")), got, 0.0, 0.3);
                }
                let got = response(&hp, rate, cutoff / 8.0);
                near(&format!("{} at cutoff / 8", tag("highpass")), got, -3.0 * per_octave, 3.0);
            }
        }
    }
}

/// Bandpass peaks at cutoff at 0 dB, notch dips there, peak and the shelves turn by gain.
#[test]
fn band_notch_peak_and_shelves_act_at_cutoff() {
    for rate in RATES {
        for cutoff in [300.0, 2000.0] {
            let tag = |k: &str| format!("{k} {cutoff} Hz at {rate}");
            let bp = Settings { resonance: 0.6, ..settings(Kind::Bandpass, cutoff) };
            near(&tag("bandpass at cutoff"), response(&bp, rate, cutoff), 0.0, 0.2);
            // The loudest of a scan in eighth octaves is at cutoff.
            let scan: Vec<f64> =
                (-8..=8).map(|k| response(&bp, rate, cutoff * 2f64.powf(k as f64 / 8.0))).collect();
            let best =
                scan.iter().enumerate().fold((0, f64::MIN), |b, (i, v)| if *v > b.1 { (i, *v) } else { b });
            assert_eq!(best.0, 8, "{scan:?}");
            assert!(scan[0] < -8.0 && scan[16] < -8.0, "{scan:?}");

            let notch = settings(Kind::Notch, cutoff);
            let dip = response(&notch, rate, cutoff);
            eprintln!("{}: {dip:.1} dB", tag("notch at cutoff"));
            assert!(dip < -30.0, "{dip}");
            near(&tag("notch at cutoff / 8"), response(&notch, rate, cutoff / 8.0), 0.0, 0.3);
            near(&tag("notch at cutoff * 4"), response(&notch, rate, cutoff * 4.0), 0.0, 0.6);

            for gain in [12.0, -12.0] {
                let pk = Settings { gain, resonance: 0.5, ..settings(Kind::Peak, cutoff) };
                near(&tag(&format!("peak {gain} at cutoff")), response(&pk, rate, cutoff), gain, 0.2);
                near(
                    &tag(&format!("peak {gain} at cutoff / 8")),
                    response(&pk, rate, cutoff / 8.0),
                    0.0,
                    1.0,
                );
                let low = Settings { gain, ..settings(Kind::LowShelf, cutoff) };
                near(
                    &tag(&format!("low_shelf {gain} at cutoff / 16")),
                    response(&low, rate, cutoff / 16.0),
                    gain,
                    0.5,
                );
                near(
                    &tag(&format!("low_shelf {gain} at cutoff")),
                    response(&low, rate, cutoff),
                    gain / 2.0,
                    0.5,
                );
                near(
                    &tag(&format!("low_shelf {gain} at cutoff * 8")),
                    response(&low, rate, cutoff * 8.0),
                    0.0,
                    0.5,
                );
                let high = Settings { gain, ..settings(Kind::HighShelf, cutoff) };
                near(
                    &tag(&format!("high_shelf {gain} at cutoff / 16")),
                    response(&high, rate, cutoff / 16.0),
                    0.0,
                    0.5,
                );
                near(
                    &tag(&format!("high_shelf {gain} at cutoff")),
                    response(&high, rate, cutoff),
                    gain / 2.0,
                    0.5,
                );
                if cutoff * 8.0 < 0.3 * rate {
                    let got = response(&high, rate, cutoff * 8.0);
                    near(&tag(&format!("high_shelf {gain} at cutoff * 8")), got, gain, 0.6);
                }
            }
        }
    }
}

/// Resonance 1 raises the lowpass at cutoff to about Q = 20 (+26 dB), at both slopes.
#[test]
fn full_resonance_peaks_near_q() {
    for steep in [false, true] {
        let s = Settings { steep, resonance: 1.0, ..settings(Kind::Lowpass, 1000.0) };
        let got = response(&s, 48_000.0, 1000.0);
        near(&format!("lowpass steep {steep}, resonance 1, at cutoff"), got, 26.0, 2.5);
    }
}

/// Cutoff, resonance, type and slope jump at random every block for 60 s of noise at full
/// scale: the output stays finite and bounded.
#[test]
fn fast_modulation_stays_stable() {
    let rate = 48_000.0;
    let mut state = State::default();
    let kinds = [
        Kind::Lowpass,
        Kind::Highpass,
        Kind::Bandpass,
        Kind::Notch,
        Kind::Peak,
        Kind::LowShelf,
        Kind::HighShelf,
    ];
    let mut most = 0.0_f64;
    for b in 0..3600_i64 {
        let r = |k: i64| (noise(b, k) + 1.0) / 2.0;
        let s = Settings {
            kind: kinds[(b / 7) as usize % kinds.len()],
            steep: b % 3 == 0,
            cutoff: 20.0 * 1000f64.powf(r(1)),
            resonance: r(2),
            gain: -24.0 + 48.0 * r(3),
            mix: 1.0,
        };
        let x: Vec<f64> = (0..800).map(|i| noise(b * 800 + i, 9)).collect();
        let out = state.run(&chans(rate, &[("ch1", x)]), &design(&s, rate)).unwrap();
        let y = &out.channels()[0].samples;
        assert!(y.iter().all(|v| v.is_finite()), "block {b}");
        most = most.max(peak(y));
    }
    eprintln!("peak over 60 s: {most:.1} ({:.1} dBFS)", db(most));
    assert!(most < 200.0, "{most}");
}

/// A sine through a lowpass whose cutoff jumps every block (bound to something at 60 fps), or
/// jumps from 100 Hz to 8 kHz at once: no sample-to-sample step beyond what a sine of the
/// output's peak makes.
#[test]
fn a_moving_cutoff_does_not_zipper() {
    let rate = 48_000.0;
    let hz = 440.0;
    let mut state = State::default();
    let mut all = Vec::new();
    // 100 Hz to 8 kHz in 2 s, in 120 steps, then a jump down and back up.
    let mut cutoffs: Vec<f64> = (0..120).map(|k| 100.0 * 80f64.powf(f64::from(k) / 119.0)).collect();
    cutoffs.extend([100.0; 20]);
    cutoffs.extend([8000.0; 20]);
    for (b, c) in cutoffs.iter().enumerate() {
        let x: Vec<f64> =
            (0..800).map(|i| (std::f64::consts::TAU * hz * (b * 800 + i) as f64 / rate).sin()).collect();
        let s = settings(Kind::Lowpass, *c);
        let out = state.run(&chans(rate, &[("ch1", x)]), &design(&s, rate)).unwrap();
        all.extend_from_slice(&out.channels()[0].samples);
    }
    let worst = |x: &[f64]| x.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f64::max);
    let sine_step = std::f64::consts::TAU * hz / rate * peak(&all);
    let (sweep, jumps) = (worst(&all[..120 * 800]), worst(&all[120 * 800..]));
    eprintln!("worst step: sweep {sweep:.4}, jumps {jumps:.4}; a sine of the peak steps {sine_step:.4}");
    assert!(sweep <= sine_step * 1.05, "{sweep} against {sine_step}");
    // The jump up turns the filter's phase shift of nearly half a wave around within 15 ms,
    // which speeds the wave up briefly; a click would step by the whole amplitude.
    assert!(jumps <= sine_step * 1.5, "{jumps} against {sine_step}");
}

/// Channels keep their own state by name, in any order; a new name starts from silence.
#[test]
fn channels_keep_their_state_by_name() {
    let rate = 48_000.0;
    let t = design(&settings(Kind::Lowpass, 500.0), rate);
    let a: Vec<f64> = (0..1600).map(|i| noise(i, 1)).collect();
    let b: Vec<f64> = (0..1600).map(|i| noise(i, 2)).collect();
    let alone = |x: &[f64]| {
        let mut s = State::default();
        let mut y =
            s.run(&chans(rate, &[("x", x[..800].to_vec())]), &t).unwrap().channels()[0].samples.clone();
        y.extend(s.run(&chans(rate, &[("x", x[800..].to_vec())]), &t).unwrap().channels()[0].samples.clone());
        y
    };
    let mut s = State::default();
    let one = s.run(&chans(rate, &[("a", a[..800].to_vec()), ("b", b[..800].to_vec())]), &t).unwrap();
    let two = s.run(&chans(rate, &[("b", b[800..].to_vec()), ("a", a[800..].to_vec())]), &t).unwrap();
    let joined = |name: &str| {
        let mut y = one.get(name).unwrap().samples.clone();
        y.extend(two.get(name).unwrap().samples.clone());
        y
    };
    assert_eq!(joined("a"), alone(&a));
    assert_eq!(joined("b"), alone(&b));
    let names: Vec<&str> = two.names().map(|n| n.as_str()).collect();
    assert_eq!(names, ["b", "a"]);
}

/// NaN and infinite samples count as 0; huge ones give finite output; any rate and any
/// cutoff, also above Nyquist or not finite, stay finite; no channels give no channels.
#[test]
fn hostile_inputs_and_params_stay_finite() {
    let rate = 48_000.0;
    let t = design(&Settings { resonance: 1.0, ..settings(Kind::Lowpass, 1000.0) }, rate);
    let mut x = sine(300.0, rate, 800);
    let mut zeroed = x.clone();
    for i in [10, 200, 201, 500] {
        x[i] = if i == 200 { f64::INFINITY } else { f64::NAN };
        zeroed[i] = 0.0;
    }
    let got = State::default().run(&chans(rate, &[("ch1", x)]), &t).unwrap();
    let want = State::default().run(&chans(rate, &[("ch1", zeroed)]), &t).unwrap();
    assert_eq!(got.channels()[0].samples, want.channels()[0].samples);

    let mut s = State::default();
    let huge = vec![1e30, -1e30, 1e300, -1e300, 1e30];
    let y = s.run(&chans(rate, &[("ch1", huge)]), &t).unwrap();
    assert!(y.channels()[0].samples.iter().all(|v| v.is_finite()));
    // Samples beyond the clip level are clipped: the state rings down within 200 ms.
    let y = s.run(&chans(rate, &[("ch1", sine(300.0, rate, 9600))]), &t).unwrap();
    assert!(y.channels()[0].samples.iter().all(|v| v.is_finite()));
    assert!(peak(&y.channels()[0].samples[7200..]) < 2.0, "the state recovers from a blast");

    for rate in [8000.0, 192_000.0, 1.0, 1e7] {
        for cutoff in [0.0, 20.0, 20_000.0, 1e9, f64::NAN, f64::INFINITY, -5.0] {
            for kind in [Kind::Lowpass, Kind::Highpass, Kind::Peak, Kind::LowShelf, Kind::HighShelf] {
                let s = Settings {
                    kind,
                    steep: true,
                    cutoff,
                    resonance: f64::NAN,
                    gain: f64::INFINITY,
                    mix: f64::NAN,
                };
                let c = design(&s, rate);
                assert!(c.iter().all(|v| v.is_finite()), "{rate} {cutoff} {kind:?}: {c:?}");
                let y = State::default().run(&chans(rate, &[("ch1", vec![1.0; 256])]), &c).unwrap();
                assert!(
                    y.channels()[0].samples.iter().all(|v| v.is_finite() && v.abs() < 100.0),
                    "{rate} {cutoff}"
                );
            }
        }
    }
    let empty = Chans::new(rate, Vec::new()).unwrap();
    assert!(State::default().run(&empty, &t).unwrap().channels().is_empty());

    // Nothing wired is an error that says what to do; hostile params are clamped.
    let mut f = F::new(&[create("/f", FILTER)]);
    assert!(f.cook("/f").unwrap_err().contains("connect"));
    let mut f = F::new(&[
        create("/o", "audio.oscillator"),
        set("/o.rate", 8000),
        create("/f", FILTER),
        set("/f.cutoff", 20_000.0),
        set("/f.resonance", 1.0),
        set("/f.type", "high_shelf"),
        set("/f.gain", 24.0),
        connect("/o", "/f", 0),
    ]);
    let mut all = f.block("/f");
    run(&mut f, "/f", 30, &mut all);
    assert!(all.iter().all(|v| v.is_finite()));
}

/// The block contract in the graph: a recook at the same tick filters the same block again
/// from its start, so after edits there and back the output and what follows are as if
/// nothing happened.
#[test]
fn a_recook_at_the_same_tick_filters_from_the_block_start() {
    let ops = |path: &str| {
        vec![
            create("/o", "audio.oscillator"),
            set("/o.shape", "noise"),
            set("/o.amplitude", 1.0),
            create(path, FILTER),
            set(&format!("{path}.resonance"), 0.6),
            connect("/o", path, 0),
        ]
    };
    let mut reference = F::new(&ops("/f"));
    let mut f = F::new(&ops("/f"));
    let mut want = reference.block("/f");
    run(&mut reference, "/f", 3, &mut want);
    let mut got = f.block("/f");
    run(&mut f, "/f", 1, &mut got);
    let before = got.len();
    f.edit(&[set("/f.cutoff", 300.0)]);
    let changed = f.block("/f");
    assert_ne!(changed, got[got.len() - 800..], "the edit shows at once");
    f.edit(&[set("/f.cutoff", 1000.0)]);
    let back = f.block("/f");
    assert_eq!(back, got[before - 800..], "back to the same params, the same block");
    run(&mut f, "/f", 2, &mut got);
    assert_eq!(got, want, "the state did not run twice");
}

/// A device input that delivered nothing hands on the same block: the filter hands on its
/// last output, the very same result, also while its cutoff moves; the next block continues
/// from where the last ended.
#[test]
fn a_repeated_device_block_is_not_filtered_twice() {
    let mut f = F::new(&[
        create("/in", "audio.device_in"),
        set("/in.device", "test"),
        create("/f", FILTER),
        connect("/in", "/f", 0),
    ]);
    f.cooker.provide(Hub::new());
    let hub = f.cooker.resource::<Hub>().unwrap().clone();
    let id = f.net.resolve(&"/f".parse().unwrap()).unwrap();
    let cook = |f: &mut F| -> Option<Data> {
        f.cooker.cook(&f.net, f.clock.now(), &[id], &[]);
        f.cooker.output(Port { node: id, output: 0 }).cloned()
    };
    let out = |f: &mut F| cook(f).unwrap();
    let a: Vec<f32> = (0..1000).map(|i| noise(i, 3) as f32).collect();
    let b: Vec<f32> = (1000..1500).map(|i| noise(i, 3) as f32).collect();
    // The input starts reading at what arrives after its first cook.
    hub.feed("test", 48_000, 1, &[0.0; 16]).unwrap();
    f.clock.advance(1);
    assert!(cook(&mut f).is_none());
    hub.feed("test", 48_000, 1, &a).unwrap();
    f.clock.advance(1);
    let first = out(&mut f);
    assert_eq!(first.get::<Chans>().unwrap().len(), 1000);
    for i in 0..4 {
        f.clock.advance(1);
        f.edit(&[set("/f.cutoff", 500.0 + 100.0 * f64::from(i))]);
        assert!(out(&mut f).same(&first), "frame {i}: filtered again");
    }
    hub.feed("test", 48_000, 1, &b).unwrap();
    f.clock.advance(1);
    let second = out(&mut f);
    assert_eq!(second.get::<Chans>().unwrap().len(), 500);
    // As one filter that saw the two blocks one after the other.
    let to64 = |x: &[f32]| x.iter().map(|v| f64::from(*v)).collect::<Vec<_>>();
    let mut s = State::default();
    let t0 = design(&settings(Kind::Lowpass, 1000.0), 48_000.0);
    let want0 = s.run(&chans(48_000.0, &[("ch1", to64(&a))]), &t0).unwrap();
    let t1 = design(&settings(Kind::Lowpass, 800.0), 48_000.0);
    let want1 = s.run(&chans(48_000.0, &[("ch1", to64(&b))]), &t1).unwrap();
    assert_eq!(first.get::<Chans>().unwrap().channels()[0].samples, want0.channels()[0].samples);
    assert_eq!(second.get::<Chans>().unwrap().channels()[0].samples, want1.channels()[0].samples);
}

/// Listening file in the listening folder: a pad chord and noise through a resonant lowpass whose cutoff an lfo
/// sweeps 100 Hz -> 8 kHz -> 100 Hz over 12 s, then 3 s of a peak and 3 s of a high shelf
/// as the sweep goes on. Run with
/// `cargo test -p penumbra-ops write_filter_files -- --ignored`.
#[test]
#[ignore]
fn write_filter_files() {
    let dir = std::path::Path::new(LISTEN_DIR);
    std::fs::create_dir_all(dir).unwrap();
    // cutoff = 100 * 80^x, x a triangle from 0 to 1 and back in 12 s.
    // A pad chord (F major: F2 A2 C3, three gates held open) with a quiet wind-like noise.
    let mut ops = super::instrument_tests::lanes("/g", 3);
    ops.extend((0..3).map(|k| set(&format!("/g.value{k}"), 1.0)));
    ops.extend([
        create("/a", "audio.ambience"),
        set("/a.preset", "pad"),
        set("/a.key", "F"),
        set("/a.octave", 2),
        set("/a.scale", "major"),
        set("/a.spread", 2),
        set("/a.brightness", 1.0),
        set("/a.level", -18.0),
        connect("/g", "/a", 0),
        create("/n", "audio.ambience"),
        set("/n.preset", "noise"),
        set("/n.brightness", 0.6),
        set("/n.level", -18.0),
        create("/x", "audio.mix"),
        connect("/a", "/x", 0),
        connect("/n", "/x", 1),
        create("/l", "chan.lfo"),
        set("/l.shape", "triangle"),
        set("/l.rate", 1.0 / 12.0),
        set("/l.amplitude", 0.5),
        set("/l.offset", 0.5),
        create("/k", "chan.constant"),
        set("/k.name0", "hz"),
        set("/k.value0", 80.0),
        create("/p", "chan.math"),
        set("/p.op", "power"),
        connect("/k", "/p", 0),
        connect("/l", "/p", 1),
        create("/m", "chan.math"),
        set("/m.op", "multiply"),
        set("/m.b", 100.0),
        connect("/p", "/m", 0),
        create("/f", FILTER),
        set("/f.resonance", 0.6),
        connect("/x", "/f", 0),
        Op::Bind {
            param: ParamPath::parse("/f.cutoff").unwrap(),
            component: None,
            from: "/m".parse().unwrap(),
            channel: Some(Name::new("hz").unwrap()),
        },
    ]);
    let mut f = F::new(&ops);
    let mut all = f.block("/f");
    run(&mut f, "/f", 12 * 60, &mut all);
    f.edit(&[set("/f.type", "peak"), set("/f.gain", 12.0)]);
    run(&mut f, "/f", 3 * 60, &mut all);
    f.edit(&[set("/f.type", "high_shelf"), set("/f.gain", -18.0), set("/f.resonance", 0.2)]);
    run(&mut f, "/f", 3 * 60, &mut all);
    eprintln!("filter_sweep: peak {:.1} dBFS", db(peak(&all)));
    assert!(all.iter().all(|v| v.is_finite()));
    write_wav(&dir.join("filter_sweep.wav"), &all);
}

/// The cost of filtering 8 channels at 48 kHz, one 60 fps frame (800 samples) per cook, both
/// slopes. Run with
/// `cargo test --release -p penumbra-ops measure_filter_cost -- --ignored --nocapture`.
#[test]
#[ignore]
fn measure_filter_cost() {
    let rate = 48_000.0;
    let names = ["c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8"];
    for steep in [false, true] {
        let mut state = State::default();
        let frames = 3600;
        let t = std::time::Instant::now();
        for b in 0..frames {
            let chs: Vec<(&str, Vec<f64>)> =
                names.iter().map(|n| (*n, (0..800).map(|i| noise(b * 800 + i, 5)).collect())).collect();
            let c = 200.0 * (1.0 + (b % 120) as f64);
            let s = Settings { steep, resonance: 0.6, ..settings(Kind::Lowpass, c) };
            let out = state.run(&chans(rate, &chs), &design(&s, rate)).unwrap();
            std::hint::black_box(out);
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0 / frames as f64;
        eprintln!(
            "cost: 8 channels at 48 kHz, steep {steep}: {ms:.3} ms per 60 fps frame (16.7 ms), input made included"
        );
    }
}
