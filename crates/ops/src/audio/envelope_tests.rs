//! `chan.audio_envelope`: levels of a sine, attack and release times, silence, the block contract
//! (a recook, a device that delivered nothing), hostile input.

use penumbra_audio::{Detect, Envelope, Hub};
use penumbra_core::{Chans, Value};
use penumbra_graph::{Data, Port};

use super::envelope::{ENVELOPE, Scale};
use super::tests::{F, connect, create, set};

/// Cooks `path` and hands its first output's channels.
pub(super) fn chans(f: &mut F, path: &str) -> Chans {
    let id = f.net.resolve(&path.parse().unwrap()).unwrap();
    f.cooker.cook(&f.net, f.clock.now(), &[id], &[]);
    if let Some(e) = f.cooker.status(id).and_then(|s| s.error) {
        panic!("{path}: {e}");
    }
    f.cooker.output(Port { node: id, output: 0 }).and_then(|d| d.get::<Chans>()).unwrap().clone()
}

/// The first sample of the first channel after `frames` frames.
fn value_after(f: &mut F, frames: usize) -> f64 {
    let mut v = chans(f, "/e").channels()[0].samples[0];
    for _ in 0..frames {
        f.clock.advance(1);
        v = chans(f, "/e").channels()[0].samples[0];
    }
    v
}

/// A 1 kHz sine at `amplitude` into an envelope with `params`.
fn sine_rig(amplitude: f64, params: &[(&str, Value)]) -> F {
    let mut ops = vec![
        create("/o", "audio.oscillator"),
        set("/o.frequency", 1000.0),
        set("/o.amplitude", amplitude),
        create("/e", ENVELOPE),
        connect("/o", "/e", 0),
    ];
    ops.extend(params.iter().map(|(k, v)| set(&format!("/e.{k}"), v.clone())));
    F::new(&ops)
}

fn db(v: f64) -> f64 {
    20.0 * v.log10()
}

#[test]
fn a_sine_at_minus_20_dbfs_reads_its_peak_and_rms() {
    let peak = value_after(&mut sine_rig(0.1, &[("detect", "peak".into())]), 60);
    assert!(db(peak / 0.1).abs() < 0.2, "peak linear {peak}");
    let peak_db = value_after(&mut sine_rig(0.1, &[("detect", "peak".into()), ("unit", "db".into())]), 60);
    assert!((peak_db + 20.0).abs() < 0.2, "peak {peak_db} dB");
    let rms_db = value_after(&mut sine_rig(0.1, &[("unit", "db".into())]), 60);
    assert!((rms_db + 23.01).abs() < 0.2, "rms {rms_db} dB");
    // level: 0 at the floor (-60) to 1 at full scale.
    let level = value_after(&mut sine_rig(0.1, &[("unit", "level".into())]), 60);
    assert!((level - (1.0 - 23.01 / 60.0)).abs() < 0.005, "level {level}");
    // One channel per input channel, named as it.
    let c = chans(&mut sine_rig(0.1, &[]), "/e");
    assert_eq!(c.names().map(|n| n.as_str()).collect::<Vec<_>>(), ["ch1"]);
    assert_eq!(c.len(), 1);
}

/// The time from a step to 63 % of the way, in seconds, at 48 kHz, for a 1 kHz sine switched
/// on (`on`) or off after a second.
fn step_time(detect: Detect, attack: f64, release: f64, on: bool) -> f64 {
    let rate = 48_000.0;
    let sine = |i: usize| 0.5 * (std::f64::consts::TAU * 1000.0 * i as f64 / rate).sin();
    let mut e = Envelope::default();
    let before: Vec<f64> = (0..48_000).map(|i| if on { 0.0 } else { sine(i) }).collect();
    let from = e.run(&before, rate, attack, release, detect);
    let target = if on {
        let mut probe = Envelope::default();
        probe.run(&(0..96_000).map(sine).collect::<Vec<_>>(), rate, attack, release, detect)
    } else {
        0.0
    };
    let goal = from + (target - from) * (1.0 - (-1.0f64).exp());
    for i in 0..480_000 {
        let x = if on { sine(48_000 + i) } else { 0.0 };
        let v = e.run(&[x], rate, attack, release, detect);
        if (on && v >= goal) || (!on && v <= goal) {
            return i as f64 / rate;
        }
    }
    f64::INFINITY
}

#[test]
fn attack_and_release_are_time_constants() {
    for detect in [Detect::Rms, Detect::Peak] {
        for (attack, release) in [(0.01, 0.2), (0.005, 0.3), (0.05, 1.0)] {
            let up = step_time(detect, attack, release, true);
            let down = step_time(detect, attack, release, false);
            assert!((up / attack - 1.0).abs() < 0.1, "{detect:?} attack {attack}: {up}");
            assert!((down / release - 1.0).abs() < 0.1, "{detect:?} release {release}: {down}");
        }
    }
}

#[test]
fn silence_goes_to_the_floor() {
    for (unit, want) in [("linear", 0.0), ("db", -60.0), ("level", 0.0)] {
        let mut f = sine_rig(0.5, &[("unit", unit.into()), ("release", Value::Float(0.05))]);
        value_after(&mut f, 10);
        f.edit(&[set("/o.amplitude", 0.0)]);
        // 60 dB in 50 ms steps of 8.7 dB: well under a second.
        let v = value_after(&mut f, 60);
        assert_eq!(v, want, "{unit}");
    }
    let mut f = sine_rig(0.5, &[("unit", "db".into()), ("floor", Value::Float(-100.0))]);
    f.edit(&[set("/e.floor", -100.0), set("/o.amplitude", 0.0)]);
    assert_eq!(value_after(&mut f, 2), -100.0);
    assert_eq!(Scale::Db.apply(f64::NAN, -60.0), -60.0);
    assert_eq!(Scale::Level.apply(10.0, -60.0), 1.0);
    assert_eq!(Scale::Linear.apply(1e-4, -60.0), 0.0);
    assert_eq!(Scale::Linear.apply(0.01, f64::NAN), 0.01);
}

/// A recook at the same tick follows the same block again from its start: edits there and back
/// give the same value, and what follows is as if nothing happened.
#[test]
fn a_recook_at_the_same_tick_gives_the_same_value() {
    let ops = |f: &str| {
        vec![
            create("/o", "audio.oscillator"),
            set("/o.shape", "noise"),
            set("/o.amplitude", 0.5),
            create(f, ENVELOPE),
            connect("/o", f, 0),
        ]
    };
    let mut reference = F::new(&ops("/e"));
    let mut f = F::new(&ops("/e"));
    let want = value_after(&mut reference, 5);
    let a = value_after(&mut f, 2);
    f.edit(&[set("/e.unit", "db")]);
    let changed = value_after(&mut f, 0);
    assert!((changed - db(a)).abs() < 1e-9, "{changed} vs {a}");
    f.edit(&[set("/e.unit", "linear")]);
    assert_eq!(value_after(&mut f, 0), a);
    assert_eq!(value_after(&mut f, 0), a, "again");
    assert_eq!(value_after(&mut f, 3), want, "the state did not run twice");
}

/// A device that delivered nothing hands on the same block: params change, the block is not
/// followed twice; the next block continues from where the first ended.
#[test]
fn a_repeated_device_block_is_not_counted_twice() {
    let mut f = F::new(&[
        create("/in", "audio.device_in"),
        set("/in.device", "test"),
        create("/e", ENVELOPE),
        set("/e.attack", 0.05),
        connect("/in", "/e", 0),
    ]);
    f.cooker.provide(Hub::new());
    let hub = f.cooker.resource::<Hub>().unwrap().clone();
    let id = f.net.resolve(&"/e".parse().unwrap()).unwrap();
    let cook = |f: &mut F| -> Option<Data> {
        f.cooker.cook(&f.net, f.clock.now(), &[id], &[]);
        f.cooker.output(Port { node: id, output: 0 }).cloned()
    };
    let value = |d: &Data| d.get::<Chans>().unwrap().channels()[0].samples[0];
    let a: Vec<f32> = (0..1000).map(|i| 0.3 * (i as f32 * 0.13).sin()).collect();
    let b: Vec<f32> = (1000..1500).map(|i| 0.3 * (i as f32 * 0.13).sin()).collect();
    hub.feed("test", 48_000, 2, &[0.0; 16]).unwrap();
    f.clock.advance(1);
    assert!(cook(&mut f).is_none());
    // Two channels, interleaved: the second half as loud.
    let stereo = |x: &[f32]| x.iter().flat_map(|v| [*v, *v / 2.0]).collect::<Vec<f32>>();
    hub.feed("test", 48_000, 2, &stereo(&a)).unwrap();
    f.clock.advance(1);
    let first = cook(&mut f).unwrap();
    let v1 = value(&first);
    let c = first.get::<Chans>().unwrap();
    assert_eq!(c.names().map(|n| n.as_str()).collect::<Vec<_>>(), ["ch1", "ch2"]);
    assert!((c.channels()[1].samples[0] / v1 - 0.5).abs() < 1e-6);
    for (i, unit) in ["db", "level", "linear"].iter().enumerate() {
        f.clock.advance(1);
        f.edit(&[set("/e.unit", *unit)]);
        let v = value(&cook(&mut f).unwrap());
        let want = Scale::parse(unit).apply(v1, -60.0);
        assert!((v - want).abs() < 1e-12, "frame {i}: {v} vs {want}");
    }
    hub.feed("test", 48_000, 2, &stereo(&b)).unwrap();
    f.clock.advance(1);
    let second = value(&cook(&mut f).unwrap());
    let to64 = |x: &[f32]| x.iter().map(|v| f64::from(*v)).collect::<Vec<_>>();
    let mut e = Envelope::default();
    let w1 = e.run(&to64(&a), 48_000.0, 0.05, 0.2, Detect::Rms);
    let w2 = e.run(&to64(&b), 48_000.0, 0.05, 0.2, Detect::Rms);
    assert!((v1 - w1).abs() < 1e-12 && (second - w2).abs() < 1e-12, "{v1} {w1}, {second} {w2}");
}

#[test]
fn hostile_samples_and_params_stay_finite() {
    let mut f = F::new(&[
        create("/in", "audio.device_in"),
        set("/in.device", "test"),
        create("/e", ENVELOPE),
        set("/e.attack", 0.0),
        set("/e.release", 1e30),
        set("/e.floor", -1e9),
        connect("/in", "/e", 0),
    ]);
    f.cooker.provide(Hub::new());
    let hub = f.cooker.resource::<Hub>().unwrap().clone();
    hub.feed("test", 48_000, 1, &[0.0; 16]).unwrap();
    f.clock.advance(1);
    let id = f.net.resolve(&"/e".parse().unwrap()).unwrap();
    f.cooker.cook(&f.net, f.clock.now(), &[id], &[]);
    for (i, x) in [f32::NAN, f32::INFINITY, 1e30, -1e30, f32::MIN_POSITIVE].into_iter().enumerate() {
        hub.feed("test", 48_000, 1, &[x; 64]).unwrap();
        f.clock.advance(1);
        let v = chans(&mut f, "/e").channels()[0].samples[0];
        assert!(v.is_finite() && v <= penumbra_audio::envelope::CLIP, "{i}: {v}");
    }
    // Sample rates far apart.
    for rate in [8000_i64, 192_000] {
        let mut f = sine_rig(0.1, &[("detect", "peak".into())]);
        f.edit(&[set("/o.rate", rate)]);
        let v = value_after(&mut f, 30);
        assert!(db(v / 0.1).abs() < 0.3, "{rate}: {v}");
    }
    let mut f = F::new(&[create("/e", ENVELOPE)]);
    let id = f.net.resolve(&"/e".parse().unwrap()).unwrap();
    f.cooker.cook(&f.net, f.clock.now(), &[id], &[]);
    let e = f.cooker.status(id).and_then(|s| s.error).unwrap();
    assert!(e.contains("connect sound"), "{e}");
}

/// The cost of following 8 channels at 48 kHz, one 60 fps frame (800 samples) per cook. Run
/// with `cargo test --release -p penumbra-ops measure_envelope_cost -- --ignored --nocapture`.
#[test]
#[ignore]
fn measure_envelope_cost() {
    let block: Vec<f64> = (0..800).map(|i| (i as f64 * 0.1).sin()).collect();
    for detect in [Detect::Rms, Detect::Peak] {
        let mut env = [Envelope::default(); 8];
        let frames = 20_000;
        let t = std::time::Instant::now();
        for _ in 0..frames {
            for e in &mut env {
                std::hint::black_box(e.run(std::hint::black_box(&block), 48_000.0, 0.01, 0.2, detect));
            }
        }
        let us = t.elapsed().as_secs_f64() * 1e6 / f64::from(frames);
        eprintln!("cost: 8 channels at 48 kHz, {detect:?}: {us:.1} us per 60 fps frame");
    }
}
