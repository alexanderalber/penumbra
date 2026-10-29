//! `chan.audio_spectrum` on the CPU: sines in their bands, a sweep across them, smoothing, the
//! block contract, hostile params. The texture is tested on the GPU in `tex::tests`.

use penumbra_audio::Hub;
use penumbra_core::{Chans, SampleAxis, Value};
use penumbra_graph::{Data, Op, Port};

use super::envelope_tests::chans;
use super::spectrum::{SPECTRUM, bands};
use super::tests::{F, connect, create, set};

/// The band a frequency falls in, for log bands from 30 Hz to 16 kHz.
fn band_of(hz: f64, count: usize) -> usize {
    (count as f64 * (hz / 30.0).ln() / (16_000.0f64 / 30.0).ln()).floor() as usize
}

fn sine(path: &str, hz: f64, amplitude: f64) -> Vec<Op> {
    vec![
        create(path, "audio.oscillator"),
        set(&format!("{path}.frequency"), hz),
        set(&format!("{path}.amplitude"), amplitude),
    ]
}

fn rig(source: Vec<Op>, from: &str, params: &[(&str, Value)]) -> F {
    let mut ops = source;
    ops.push(create("/s", SPECTRUM));
    ops.push(connect(from, "/s", 0));
    ops.extend(params.iter().map(|(k, v)| set(&format!("/s.{k}"), v.clone())));
    F::new(&ops)
}

/// The bands after `frames` frames.
fn bands_after(f: &mut F, frames: usize) -> Vec<f64> {
    for _ in 0..frames {
        chans(f, "/s");
        f.clock.advance(1);
    }
    let c = chans(f, "/s");
    assert_eq!(c.axis(), SampleAxis::Index);
    assert_eq!(c.names().map(|n| n.as_str()).collect::<Vec<_>>(), ["spectrum"]);
    c.channels()[0].samples.clone()
}

fn loudest(b: &[f64]) -> usize {
    b.iter().copied().enumerate().fold((0, f64::MIN), |m, c| if c.1 > m.1 { c } else { m }).0
}

#[test]
fn a_sine_lands_in_its_band_at_its_level() {
    for hz in [100.0, 440.0, 3000.0, 9000.0] {
        let mut f = rig(sine("/o", hz, 0.1), "/o", &[("unit", "db".into())]);
        let b = bands_after(&mut f, 10);
        assert_eq!(b.len(), 32);
        assert_eq!(loudest(&b), band_of(hz, 32), "{hz} Hz: {b:?}");
        // Low bands are narrower than the sine's spread over the transform's bins: its power
        // is shared with the neighbours. Together they read the sine's level.
        let k = loudest(&b);
        let power: f64 = b[k.saturating_sub(1)..=(k + 1).min(31)].iter().map(|v| 10f64.powf(v / 10.0)).sum();
        let together = 10.0 * power.log10();
        assert!((together + 20.0).abs() < 0.5, "{hz} Hz reads {together} dB around band {k}");
        if hz >= 400.0 {
            assert!((b[k] + 20.0).abs() < 1.0, "{hz} Hz reads {} dB in its band", b[k]);
        }
        // Far from the sine only leakage, 40 dB down at least.
        let far = if band_of(hz, 32) > 16 { 0 } else { 31 };
        assert!(b[far] < -60.0 + 1e-9 || b[far] < b[loudest(&b)] - 40.0, "{hz} Hz: band {far} {}", b[far]);
    }
}

#[test]
fn two_sines_make_two_peaks() {
    let mut ops = sine("/a", 200.0, 0.2);
    ops.extend(sine("/b", 4000.0, 0.05));
    ops.extend([create("/m", "audio.mix"), connect("/a", "/m", 0), connect("/b", "/m", 1)]);
    let mut f = rig(ops, "/m", &[("unit", "db".into()), ("bands", Value::Int(64))]);
    let b = bands_after(&mut f, 10);
    let (i, j) = (band_of(200.0, 64), band_of(4000.0, 64));
    for k in [i, j] {
        assert!(b[k] > b[k - 2] + 20.0 && b[k] > b[k + 2] + 20.0, "band {k} is no peak: {b:?}");
    }
    assert!((b[i] + 14.0).abs() < 1.5 && (b[j] + 26.0).abs() < 1.5, "{} {}", b[i], b[j]);
    // Linear spacing: the same two sines, in bands by Hz.
    f.edit(&[
        set("/a.frequency", 250.0),
        set("/b.frequency", 4050.0),
        set("/s.spacing", "linear"),
        set("/s.min", 0.0),
        set("/s.max", 6401.0),
        set("/s.bands", 64),
    ]);
    let b = bands_after(&mut f, 10);
    // 100 Hz per band from 1 Hz (min clamps there).
    assert_eq!(loudest(&b[..32]), 2, "{b:?}");
    assert_eq!(32 + loudest(&b[32..]), 40, "{b:?}");
    assert!(b[20] == -60.0 && b[55] == -60.0, "{b:?}");
}

#[test]
fn a_log_sweep_moves_across_the_bands() {
    let mut f = rig(sine("/o", 40.0, 0.3), "/o", &[]);
    let mut last = 0;
    let mut seen = Vec::new();
    for k in 0..=40 {
        let hz = 40.0 * (12_000.0f64 / 40.0).powf(f64::from(k) / 40.0);
        f.edit(&[set("/o.frequency", hz)]);
        let b = bands_after(&mut f, 6);
        let i = loudest(&b);
        assert!(i >= last, "{hz:.0} Hz: band {i} after {last}");
        assert!(i.abs_diff(band_of(hz, 32)) <= 1, "{hz:.0} Hz: band {i}, want {}", band_of(hz, 32));
        last = i;
        seen.push(i);
    }
    assert!(seen.first().unwrap() < &3 && seen.last().unwrap() > &28, "{seen:?}");
}

/// release: a band falls to about a third in the release time once its sine stops; attack 0
/// rises at once.
#[test]
fn bands_rise_at_once_and_fall_over_release() {
    let mut f =
        rig(sine("/o", 1000.0, 0.5), "/o", &[("unit", "linear".into()), ("release", Value::Float(0.5))]);
    let k = band_of(1000.0, 32);
    let on = bands_after(&mut f, 10)[k];
    f.edit(&[set("/o.amplitude", 0.0)]);
    // The window (85 ms) empties first; from then on, the fall is the release.
    let empty = bands_after(&mut f, 8)[k];
    let later = bands_after(&mut f, 30)[k];
    let ratio = later / empty;
    assert!((ratio - (-1.0f64).exp()).abs() < 0.05, "{on} {empty} {later}: {ratio}");
}

/// A device that delivered nothing hands on the same block: params change, the band values
/// follow the params, but nothing is pushed or smoothed twice.
#[test]
fn a_repeated_block_is_not_counted_twice() {
    let ops = |release: f64| {
        vec![
            create("/in", "audio.device_in"),
            set("/in.device", "test"),
            create("/s", SPECTRUM),
            set("/s.release", release),
            set("/s.unit", "linear"),
            connect("/in", "/s", 0),
        ]
    };
    let run = |edits: bool| -> Vec<Vec<f64>> {
        let mut f = F::new(&ops(0.3));
        f.cooker.provide(Hub::new());
        let hub = f.cooker.resource::<Hub>().unwrap().clone();
        let id = f.net.resolve(&"/s".parse().unwrap()).unwrap();
        let cook = |f: &mut F| -> Option<Data> {
            f.cooker.cook(&f.net, f.clock.now(), &[id], &[]);
            f.cooker.output(Port { node: id, output: 0 }).cloned()
        };
        hub.feed("test", 48_000, 1, &[0.0; 16]).unwrap();
        f.clock.advance(1);
        assert!(cook(&mut f).is_none());
        let mut got = Vec::new();
        for block in 0..6 {
            let amp = if block < 3 { 0.5 } else { 0.0 };
            let x: Vec<f32> =
                (block * 800..block * 800 + 800).map(|i| amp * (i as f32 * 0.3).sin()).collect();
            hub.feed("test", 48_000, 1, &x).unwrap();
            f.clock.advance(1);
            let first = cook(&mut f).unwrap();
            if edits {
                for unit in ["db", "level", "linear"] {
                    f.clock.advance(1);
                    f.edit(&[set("/s.unit", unit)]);
                    cook(&mut f).unwrap();
                }
                let again = cook(&mut f).unwrap();
                assert!(
                    again.get::<Chans>().unwrap().channels()[0].samples
                        == first.get::<Chans>().unwrap().channels()[0].samples
                );
            }
            got.push(first.get::<Chans>().unwrap().channels()[0].samples.clone());
        }
        got
    };
    assert_eq!(run(true), run(false));
}

#[test]
fn hostile_params_clamp() {
    let mut f = rig(sine("/o", 1000.0, 0.5), "/o", &[("bands", Value::Int(1))]);
    assert_eq!(bands_after(&mut f, 2).len(), 4);
    f.edit(&[set("/s.bands", 10_000)]);
    assert_eq!(bands_after(&mut f, 2).len(), 512);
    // min above max: the two swap.
    f.edit(&[set("/s.bands", 32), set("/s.min", 16_000.0), set("/s.max", 30.0)]);
    let b = bands_after(&mut f, 8);
    assert_eq!(loudest(&b), band_of(1000.0, 32));
    // max above Nyquist: the spectrum ends there, with a warning.
    f.edit(&[
        set("/o.rate", 16_000),
        set("/o.frequency", 3000.0),
        set("/s.min", 30.0),
        set("/s.max", 16_000.0),
    ]);
    let b = bands_after(&mut f, 8);
    let id = f.net.resolve(&"/s".parse().unwrap()).unwrap();
    let w = f.cooker.status(id).and_then(|s| s.warning).unwrap_or_default();
    assert!(w.contains("8000 Hz"), "{w}");
    let want = (32.0 * (3000.0f64 / 30.0).ln() / (8000.0f64 / 30.0).ln()).floor() as usize;
    assert_eq!(loudest(&b), want, "{b:?}");
    assert!(b.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
    // Equal min and max, NaN-free edges.
    let (b, _) = bands(32, 500.0, 500.0, true, 48_000.0);
    assert!(b.low < b.high, "{b:?}");
    let (b, _) = bands(32, 1.0, 1.0, true, 48_000.0);
    assert!(b.low < b.high && b.low > 0.0, "{b:?}");
}

#[test]
fn hostile_samples_stay_finite() {
    let mut f = F::new(&[
        create("/in", "audio.device_in"),
        set("/in.device", "test"),
        create("/s", SPECTRUM),
        set("/s.attack", 1e30),
        set("/s.unit", "db"),
        connect("/in", "/s", 0),
    ]);
    f.cooker.provide(Hub::new());
    let hub = f.cooker.resource::<Hub>().unwrap().clone();
    hub.feed("test", 48_000, 1, &[0.0; 16]).unwrap();
    f.clock.advance(1);
    let id = f.net.resolve(&"/s".parse().unwrap()).unwrap();
    f.cooker.cook(&f.net, f.clock.now(), &[id], &[]);
    for x in [f32::NAN, f32::INFINITY, 1e30, -1e30] {
        hub.feed("test", 48_000, 1, &[x; 700]).unwrap();
        f.clock.advance(1);
        let b = chans(&mut f, "/s").channels()[0].samples.clone();
        assert!(b.iter().all(|v| v.is_finite() && (-60.0..=30.0).contains(v)), "{x}: {b:?}");
    }
}
