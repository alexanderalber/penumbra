//! MIDI end to end, with bytes injected as a device would send them (`midi.inject`), so no
//! device is needed.

use std::time::Duration;

use serde_json::json;

use super::{Fixture, eventually};

fn inject(f: &Fixture, bytes: &[u8]) {
    f.call("midi.inject", json!({"device": "Test Pads", "bytes": bytes}));
}

fn value(f: &Fixture, path: &str, name: &str) -> Option<f64> {
    let read = f.call("chan.read", json!({"path": path}));
    read["channels"].as_array()?.iter().find(|c| c["name"] == name)?["value"].as_f64()
}

#[test]
fn controls_become_channels_learn_binds_them_and_the_clock_is_a_tempo() {
    let f = Fixture::new("midi_in");
    f.call("node.create", json!({"path": "/in", "kind": "chan.midi_in", "params": {"device": "test pads"}}));
    inject(&f, &[0xB0, 7, 127, 0x99, 36, 64, 0xE0, 0, 64]);
    eventually("the controls as channels", || value(&f, "/in", "cc_1_7") == Some(1.0));
    assert_eq!(value(&f, "/in", "note_10_36"), Some(1.0));
    assert!((value(&f, "/in", "note_10_36_vel").unwrap_or(0.0) - 64.0 / 127.0).abs() < 1e-9);
    assert_eq!(value(&f, "/in", "bend_1"), Some(0.0));

    // A tap shorter than a frame shows for one frame.
    inject(&f, &[0x89, 36, 0]);
    eventually("the pad let go", || value(&f, "/in", "note_10_36") == Some(0.0));
    f.call("node.watch", json!({"path": "/in"}));
    inject(&f, &[0x99, 36, 100, 0x89, 36, 0]);
    let mut saw = false;
    for _ in 0..100 {
        saw |= value(&f, "/in", "note_10_36") == Some(1.0);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(saw, "the tap was lost");
    eventually("down again after the tap", || value(&f, "/in", "note_10_36") == Some(0.0));

    // Learn: the next control moved binds the param.
    f.call("node.create", json!({"path": "/m", "kind": "chan.math", "params": {"op": "multiply", "b": 1}}));
    f.call("osc.learn", json!({"param": "/m.b"}));
    std::thread::sleep(Duration::from_millis(50));
    inject(&f, &[0xB0, 21, 32]);
    let mut learned = json!(null);
    eventually("the learn to end", || {
        learned = f.call("osc.status", json!({}))["learned"].clone();
        learned.is_object()
    });
    assert_eq!(learned["channels"], json!(["cc_1_21"]), "{learned}");
    assert_eq!(learned["from"], "/in", "{learned}");

    // Driver timestamps keep a 125 BPM clock independent of the test thread's scheduling.
    inject(&f, &[0xFA]);
    for i in 0..60 {
        f.engine.midi().inject("Test Pads", &[0xF8], Some(i * 20_000));
    }
    eventually("the MIDI clock as channels", || {
        value(&f, "/in", "bpm").is_some_and(|bpm| (bpm - 125.0).abs() < 0.5)
    });
    let bpm = value(&f, "/in", "bpm").unwrap_or(0.0);
    assert!((bpm - 125.0).abs() < 0.5, "{bpm}");
    assert_eq!(value(&f, "/in", "playing"), Some(1.0));
    let status = f.call("midi.status", json!({}));
    let input = &status["inputs"][0];
    assert_eq!(
        (input["name"].as_str(), input["injected"].as_bool()),
        (Some("Test Pads"), Some(true)),
        "{status}"
    );
}

#[test]
fn a_missing_device_says_which_there_are() {
    if let Some(e) = penumbra_midi::hub::list().error {
        eprintln!("skipping missing MIDI device test: {e}");
        return;
    }
    let f = Fixture::new("midi_missing");
    f.call(
        "node.create",
        json!({"path": "/in", "kind": "chan.midi_in", "params": {"device": "no such device 123"}}),
    );
    f.call("node.create", json!({"path": "/out", "kind": "out.midi", "params": {"device": ""}}));
    f.call("node.watch", json!({"path": "/in"}));
    f.call("node.watch", json!({"path": "/out"}));
    let error = |path: &str| {
        f.call("node.info", json!({"path": path}))["status"]["error"].as_str().unwrap_or("").to_string()
    };
    eventually("an error on the input", || error("/in").contains("no MIDI input"));
    eventually("an error on the output", || error("/out").contains("name the device"));
}

#[test]
fn midi_out_sends_what_changed_and_everything_when_opened_anew() {
    let f = Fixture::new("midi_out");
    f.engine.midi().capture("Test Out");
    f.call("node.create", json!({"path": "/in", "kind": "chan.midi_in", "params": {"device": "test pads"}}));
    f.call("node.create", json!({"path": "/out", "kind": "out.midi", "params": {"device": "test out"}}));
    f.call("node.connect", json!({"from": "/in", "to": "/out"}));
    f.call("node.watch", json!({"path": "/out"}));
    inject(&f, &[0xB0, 7, 127, 0x90, 36, 100]);
    let mut sent: Vec<Vec<u8>> = Vec::new();
    eventually("the controls sent back", || {
        sent.extend(f.engine.midi().captured("Test Out"));
        sent.len() >= 2
    });
    // The pad's velocity channel sends nothing; its note goes with its velocity.
    assert_eq!(sent, vec![vec![0xB0, 7, 127], vec![0x90, 36, 100]]);
    // Nothing changed: nothing more. A change: only that.
    std::thread::sleep(Duration::from_millis(200));
    inject(&f, &[0xB0, 7, 0]);
    let mut more: Vec<Vec<u8>> = Vec::new();
    eventually("the change sent", || {
        more.extend(f.engine.midi().captured("Test Out"));
        !more.is_empty()
    });
    assert_eq!(more, vec![vec![0xB0, 7, 0]]);
}
