//! Audio end to end, without a device: samples are fed into the hub as a device would send
//! them.

use std::time::{Duration, Instant};

use serde_json::{Value as Json, json};

use super::{Fixture, eventually};

/// Kicks at `bpm` on the left channel, the same at half the level on the right, `seconds` long.
fn kicks(rate: u32, bpm: f64, seconds: f64) -> Vec<f32> {
    let rate_f = f64::from(rate);
    let n = (rate_f * seconds) as usize;
    let period = 60.0 / bpm;
    let mut out = Vec::with_capacity(n * 2);
    for i in 0..n {
        let t = i as f64 / rate_f;
        let s = (t + period - 0.2).rem_euclid(period);
        let v = (std::f64::consts::TAU * 60.0 * s).sin() * (-s * 25.0).exp() * 0.5;
        out.push(v as f32);
        out.push((v * 0.5) as f32);
    }
    out
}

fn channel(read: &Json, name: &str) -> f64 {
    read["channels"].as_array().unwrap().iter().find(|c| c["name"] == name).unwrap()["value"]
        .as_f64()
        .unwrap()
}

fn error_of(f: &Fixture, path: &str) -> String {
    f.call("node.info", json!({"path": path}))["status"]["error"].as_str().unwrap_or_default().to_string()
}

#[test]
fn fed_audio_becomes_channels_and_a_tempo() {
    let f = Fixture::new("audio");
    let hub = f.engine.audio().clone();
    let rate = 48_000;
    // The stream exists before the node wants it, so no device is looked for.
    hub.feed("test", rate, 2, &[0.0; 2]).unwrap();
    f.call(
        "node.batch",
        json!({"ops": [
            {"op": "create", "path": "/in", "kind": "chan.audioin", "params": {"device": "test"}},
            {"op": "create", "path": "/a", "kind": "chan.audioanalysis"},
            {"op": "connect", "from": "/in", "to": "/a"},
        ]}),
    );
    f.call("node.watch", json!({"path": "/a"}));
    f.call("node.watch", json!({"path": "/in"}));
    // Ten seconds of audio, fed about ten times faster than real time in 0.1 s pieces; each
    // cook takes what arrived since the last.
    let samples = kicks(rate, 120.0, 10.0);
    let piece = (rate as usize / 10) * 2;
    for chunk in samples.chunks(piece) {
        hub.feed("test", rate, 2, chunk).unwrap();
        std::thread::sleep(Duration::from_millis(10));
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let read = loop {
        let read = f.call("chan.read", json!({"path": "/a"}));
        if (channel(&read, "bpm") - 120.0).abs() < 1.0 || Instant::now() > deadline {
            break read;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!((channel(&read, "bpm") - 120.0).abs() < 1.0, "{read}");
    let names: Vec<&str> =
        read["channels"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert_eq!(names, penumbra_ops::audio::FEATURES);
    let spectrum = f.call("chan.read", json!({"path": "/a", "output": "spectrum", "samples": true}));
    assert_eq!(spectrum["channels"][0]["samples"].as_array().unwrap().len(), 32, "{spectrum}");

    // The input: two channels at the device rate.
    let input = f.call("chan.read", json!({"path": "/in"}));
    let names: Vec<&str> =
        input["channels"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["ch1", "ch2"], "{input}");
    assert_eq!(input["rate"], 48_000.0, "{input}");

    // One channel picked; a missing one is an error that names those there are.
    f.call("node.set", json!({"path": "/a", "params": {"channel": "ch9"}}));
    hub.feed("test", rate, 2, &samples[..9600]).unwrap();
    eventually("the missing channel to show", || error_of(&f, "/a").contains("ch1, ch2"));
    f.call("node.set", json!({"path": "/a", "params": {"channel": "ch2"}}));
    hub.feed("test", rate, 2, &samples[..9600]).unwrap();
    eventually("the error to clear", || error_of(&f, "/a").is_empty());

    let status = f.call("audio.status", json!({}));
    let stream = &status["streams"][0];
    assert_eq!((stream["key"].as_str(), stream["fed"].as_bool()), (Some("test"), Some(true)), "{status}");
    assert_eq!(status["wanted"], json!(["test"]), "{status}");
}

#[test]
fn audio_errors_are_node_errors() {
    let f = Fixture::new("audio_errors");
    // Not audio: a 60 Hz channel.
    f.call(
        "node.batch",
        json!({"ops": [
            {"op": "create", "path": "/lfo", "kind": "chan.lfo"},
            {"op": "create", "path": "/a", "kind": "chan.audioanalysis"},
            {"op": "connect", "from": "/lfo", "to": "/a"},
        ]}),
    );
    f.call("node.watch", json!({"path": "/a"}));
    eventually("the analysis to refuse", || error_of(&f, "/a").contains("not audio"));

    // Switched off.
    f.call("audio.config", json!({"enabled": false}));
    f.call(
        "node.create",
        json!({"path": "/in", "kind": "chan.audioin", "params": {"device": "nothing like it"}}),
    );
    f.call("node.watch", json!({"path": "/in"}));
    eventually("the switch to show", || error_of(&f, "/in").contains("switched off"));
    assert!(f.call("audio.status", json!({}))["streams"].as_array().unwrap().is_empty());
    // On again, a device that is not there is an error, never a crash (on a machine without
    // audio the message differs).
    f.call("audio.config", json!({"enabled": true}));
    eventually("a missing device to show", || {
        let e = error_of(&f, "/in");
        !e.is_empty() && !e.contains("switched off") && !e.contains("yet")
    });
    // Listing devices answers whatever the machine has.
    let r = f.registry.dispatch("audio.devices", &json!({}));
    assert!(r["ok"] == true || r["error"].is_string(), "{r}");
    assert!(f.fail("audio.config", json!({"enabled": 3})).contains("bad params"));
}
