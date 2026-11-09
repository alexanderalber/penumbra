//! `chan.audio_pitch` in a network: an oscillator's tone, the channels, the block contract (a
//! recook gives the same values), a change of sample rate, and errors.

use penumbra_core::{Chans, Value};

use super::envelope_tests::chans;
use super::pitch_track::{CHANNELS, PITCH};
use super::tests::{F, connect, create, set};

fn rig(params: &[(&str, Value)]) -> F {
    let mut ops = vec![
        create("/o", "audio.oscillator"),
        set("/o.frequency", 440.0),
        set("/o.amplitude", 0.3),
        create("/p", PITCH),
        connect("/o", "/p", 0),
    ];
    ops.extend(params.iter().map(|(k, v)| set(&format!("/p.{k}"), v.clone())));
    F::new(&ops)
}

fn value(c: &Chans, name: &str) -> f64 {
    c.get(name).unwrap().samples[0]
}

/// The channels after `frames` frames at 60 fps.
fn after(f: &mut F, frames: usize) -> Chans {
    let mut c = chans(f, "/p");
    for _ in 0..frames {
        f.clock.advance(1);
        c = chans(f, "/p");
    }
    c
}

#[test]
fn an_oscillator_at_440_hz_reads_a4() {
    let mut f = rig(&[]);
    let c = after(&mut f, 20);
    assert_eq!(c.names().map(|n| n.as_str()).collect::<Vec<_>>(), CHANNELS);
    assert!((value(&c, "note") - 69.0).abs() < 0.02, "{c:?}");
    assert!((value(&c, "frequency") - 440.0).abs() < 0.5);
    assert!((value(&c, "pitch_class") - 9.0).abs() < 0.02);
    assert!(value(&c, "clarity") > 0.99);
    assert_eq!(value(&c, "sure"), 1.0);
    assert!((value(&c, "heard_frequency") - 440.0).abs() < 0.5);
    // A recook of the same block gives the same values.
    let again = chans(&mut f, "/p");
    assert_eq!(c, again);
    // A new tone moves the note; the oscillator glides over one frame.
    f.edit(&[set("/o.frequency", 220.0)]);
    let c = after(&mut f, 30);
    assert!((value(&c, "note") - 57.0).abs() < 0.05, "{c:?}");
    // Silence keeps it. The window over the tone's end pulls it a little.
    f.edit(&[set("/o.amplitude", 0.0)]);
    let c = after(&mut f, 10);
    assert!((value(&c, "note") - 57.0).abs() < 0.2, "{c:?}");
    assert_eq!(value(&c, "clarity"), 0.0);
    assert_eq!(value(&c, "sure"), 0.0);
    assert_eq!(value(&c, "heard_frequency"), 0.0);
}

#[test]
fn a_change_of_rate_starts_over_and_noise_never_moves_the_note() {
    let mut f = rig(&[("smooth", 0.0.into())]);
    let c = after(&mut f, 15);
    assert!((value(&c, "note") - 69.0).abs() < 0.02);
    f.edit(&[set("/o.rate", 96_000i64)]);
    // Starting over: nothing until the window is full again, then the same tone.
    let c = after(&mut f, 15);
    assert!((value(&c, "note") - 69.0).abs() < 0.02, "{c:?}");
    f.edit(&[set("/o.shape", "noise")]);
    // The first frames of noise still hold the tone in the window, and may pull the note a few
    // cents. From then on it stays.
    let held = value(&after(&mut f, 4), "note");
    assert!((held - 69.0).abs() < 0.1, "{held}");
    for _ in 0..60 {
        f.clock.advance(1);
        let c = chans(&mut f, "/p");
        assert_eq!(value(&c, "sure"), 0.0, "{c:?}");
        assert_eq!(value(&c, "note"), held);
    }
}

#[test]
fn errors_name_the_fix() {
    let mut f = F::new(&[create("/p", PITCH)]);
    let id = f.net.resolve(&"/p".parse().unwrap()).unwrap();
    f.cooker.cook(&f.net, f.clock.now(), &[id], &[]);
    let e = f.cooker.status(id).and_then(|s| s.error).unwrap();
    assert!(e.contains("connect sound"), "{e}");
    let mut f = rig(&[("channel", "ch7".into())]);
    let id = f.net.resolve(&"/p".parse().unwrap()).unwrap();
    f.cooker.cook(&f.net, f.clock.now(), &[id], &[]);
    let e = f.cooker.status(id).and_then(|s| s.error).unwrap();
    assert!(e.contains("no channel `ch7`"), "{e}");
}
