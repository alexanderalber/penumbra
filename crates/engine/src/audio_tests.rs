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
            {"op": "create", "path": "/in", "kind": "audio.device_in", "params": {"device": "test"}},
            {"op": "create", "path": "/a", "kind": "chan.audio_analysis"},
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
    // Not audio: a 60 Hz channel does not even connect.
    f.call(
        "node.batch",
        json!({"ops": [
            {"op": "create", "path": "/lfo", "kind": "chan.lfo"},
            {"op": "create", "path": "/a", "kind": "chan.audio_analysis"},
        ]}),
    );
    let e = f.fail("node.connect", json!({"from": "/lfo", "to": "/a"}));
    assert!(e.contains("wants Audio"), "{e}");

    // Switched off.
    f.call("audio.config", json!({"enabled": false}));
    f.call(
        "node.create",
        json!({"path": "/in", "kind": "audio.device_in", "params": {"device": "nothing like it"}}),
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

/// A project from before sound had a family of its own: `chan.audio_in` loads as
/// `audio.device_in`; its wire into the analysis stays, a wire into a chan node and a binding
/// to it are left out (they cannot carry sound), and everything else loads as it was.
#[test]
fn an_old_audio_in_opens_as_a_device_in() {
    let f = Fixture::new("audio-migration");
    let old = penumbra_core::scratch::ScratchDir::unborn("engine-audio-migration-old");
    std::fs::create_dir_all(old.path()).unwrap();
    let file = old.path().join(penumbra_project::FILE_NAME);
    let text = r#"(version: 2, nodes: {
        "/mic": (kind: "chan.audio_in", params: {"device": "test"}),
        "/a": (kind: "chan.audio_analysis", inputs: {"audio": "mic"}),
        "/m": (kind: "chan.math", params: {"b": 2.0}, inputs: {"a": "mic", "b": "a"}),
        "/t": (kind: "chan.lfo", binds: {"rate": "mic:ch1", "amplitude": "a:level"}),
    })"#;
    std::fs::write(&file, text).unwrap();
    let r = f.call("project.open", json!({"dir": old.path()}));
    assert_eq!(
        r["renamed"],
        json!([{"path": "/mic", "from": "chan.audio_in", "to": "audio.device_in"}]),
        "{r}"
    );
    let info = f.call("project.info", json!({}));
    assert_eq!(info["broken"], json!([]), "{info}");
    assert_eq!(f.call("node.info", json!({"path": "/mic"}))["kind"], "audio.device_in");
    let a = f.call("node.info", json!({"path": "/a"}));
    assert!(a["inputs"][0]["from"].as_str().unwrap().starts_with("/mic"), "{a}");
    let m = f.call("node.info", json!({"path": "/m"}));
    assert!(m["inputs"][0]["from"].is_null(), "the chan wire from the sound is left out: {m}");
    assert!(m["inputs"][1]["from"].as_str().unwrap().starts_with("/a"), "{m}");
    let t = f.call("node.info", json!({"path": "/t"}));
    let binds: Vec<&str> =
        t["binds"].as_array().unwrap().iter().map(|b| b["param"].as_str().unwrap()).collect();
    assert_eq!(binds, ["amplitude"], "{t}");
    // The new kind is written back.
    eventually("autosave to write the new name", || {
        std::fs::read_to_string(&file).unwrap().contains(r#""/mic": (kind: "audio.device_in""#)
    });

    // Fed, the analysis still listens to the renamed input.
    let hub = f.engine.audio().clone();
    f.call("node.watch", json!({"path": "/a"}));
    let samples = kicks(48_000, 120.0, 1.0);
    for chunk in samples.chunks(9600) {
        hub.feed("test", 48_000, 2, chunk).unwrap();
        std::thread::sleep(Duration::from_millis(10));
    }
    eventually("the analysis to hear it", || {
        channel(&f.call("chan.read", json!({"path": "/a"})), "level") > 0.0
    });
    let read = f.call("chan.read", json!({"path": "/mic"}));
    assert_eq!(read["kind"], "audio", "{read}");
    assert!(read["channels"][0]["peak"].as_f64().unwrap() > 0.0, "{read}");
}

/// The oscillator through a mix, read by command: one frame's worth of samples per block.
#[test]
fn an_oscillator_is_read_by_command() {
    let f = Fixture::new("audio-osc");
    f.call(
        "node.batch",
        json!({"ops": [
            {"op": "create", "path": "/o", "kind": "audio.oscillator", "params": {"amplitude": 0.5}},
            {"op": "create", "path": "/m", "kind": "audio.mix", "params": {"gain0": 0.5}},
            {"op": "connect", "from": "/o", "to": "/m"},
        ]}),
    );
    let read = f.call("chan.read", json!({"path": "/m"}));
    assert_eq!((read["kind"].as_str(), read["rate"].as_f64()), (Some("audio"), Some(48_000.0)), "{read}");
    assert_eq!(read["length"], 800, "{read}");
    let peak = read["channels"][0]["peak"].as_f64().unwrap();
    assert!((peak - 0.25).abs() < 0.01, "{read}");
    let kinds = f.call("node.kinds", json!({"kind": "audio.mix"}));
    assert!(kinds.to_string().contains("gain7"), "{kinds}");
}

/// An oscillator into `out.audio`, through the running cook thread into a bus without a
/// device: the samples pushed are the oscillator's, gapless over many frames, a stall of the
/// cook thread included.
#[test]
fn an_oscillator_plays_through_the_engine_gaplessly() {
    let f = Fixture::new("audio_out");
    let out = f.engine.audio_out().clone();
    f.call(
        "node.batch",
        json!({"ops": [
            {"op": "create", "path": "/osc", "kind": "audio.oscillator", "params": {"frequency": 440.0, "amplitude": 0.1}},
            {"op": "create", "path": "/out", "kind": "out.audio"},
            {"op": "connect", "from": "/osc", "to": "/out"},
        ]}),
    );
    eventually("the bus to get blocks", || out.pushed("").first().is_some_and(|c| c.len() > 48_000 / 4));
    // A stall of the cook thread: the frames it skipped come as one block.
    f.engine.on_cook(|_| std::thread::sleep(Duration::from_millis(60))).unwrap();
    eventually("a second of sound", || out.pushed("").first().is_some_and(|c| c.len() > 48_000));
    let pushed = out.pushed("");
    let samples = &pushed[0];
    // A steady sine of 440 Hz at 0.1 steps by at most 2 pi f A / rate per sample; a gap or an
    // overlap between blocks would jump. The first block is faded in; skip it.
    let max_step = std::f64::consts::TAU * 440.0 * 0.1 / 48_000.0;
    for (k, w) in samples.windows(2).enumerate().skip(2000) {
        let step = f64::from(w[1] - w[0]).abs();
        assert!(step <= max_step * 1.01 + 1e-6, "a jump of {step} at sample {k}");
    }
    let peak = samples.iter().skip(2000).fold(0.0f32, |m, v| m.max(v.abs()));
    assert!((f64::from(peak) - 0.1).abs() < 1e-3, "{peak}");

    let status = f.call("audio.status", json!({}));
    let bus = &status["outputs"][0];
    assert_eq!(bus["key"], "", "{status}");
    assert_eq!(bus["lanes"].as_array().unwrap().len(), 1, "{status}");
    assert_eq!(bus["offline"]["underruns"], 0, "{status}");
    assert_eq!(status["muted"], false);

    // The panic button.
    assert_eq!(f.call("audio.mute", json!({}))["muted"], true);
    assert!(out.muted());
    assert_eq!(f.call("audio.mute", json!({"mute": false}))["muted"], false);

    // Paused, the bus fades out; deleted, the device closes.
    f.call("cook.play", json!({"playing": false}));
    eventually("the pause to reach the bus", || out.bus_status("").is_some_and(|b| b.paused));
    f.call("node.delete", json!({"path": "/out"}));
    eventually("the bus to close", || out.keys().is_empty());
}

#[test]
fn node_info_and_kinds_say_when_a_param_does_nothing() {
    let f = Fixture::new("audio-applies");
    f.call(
        "node.batch",
        json!({"ops": [
            {"op": "create", "path": "/i", "kind": "audio.instrument", "params": {"preset": "kick"}},
            {"op": "create", "path": "/c", "kind": "chan.constant"},
        ]}),
    );
    let param = |info: &Json, name: &str| {
        info["params"].as_array().unwrap().iter().find(|p| p["name"] == name).unwrap().clone()
    };
    let info = f.call("node.info", json!({"path": "/i"}));
    assert_eq!(param(&info, "key")["applies"], false, "{info}");
    assert_eq!(param(&info, "key")["why"], "Does nothing with preset kick");
    assert_eq!(param(&info, "threshold")["why"], "Does something only while play is wired");
    assert!(param(&info, "decay").get("applies").is_none(), "{info}");
    f.call("node.connect", json!({"from": "/c", "to": "/i"}));
    f.call("node.set", json!({"path": "/i", "params": {"preset": "bell"}}));
    let info = f.call("node.info", json!({"path": "/i"}));
    assert!(param(&info, "key").get("applies").is_none(), "{info}");
    assert!(param(&info, "threshold").get("applies").is_none(), "{info}");
    let help = f.call("node.kinds", json!({"kind": "audio.instrument", "param": "note"}));
    assert_eq!(help["param"]["applies"], "only with preset pluck, mallet or bell", "{help}");
    let kinds = f.call("node.kinds", json!({"kind": "audio.instrument"}));
    let note = kinds["kinds"][0]["params"].as_array().unwrap().iter().find(|p| p["name"] == "note").unwrap();
    assert_eq!(note["applies"][0]["choice"]["param"], "preset", "{note}");
}

/// `audio.play`: an instrument with nothing wired plays one note through `out.audio` from the
/// next block on; paused, the press is dropped and says so; hostile args are errors.
#[test]
fn audio_play_presses_an_instrument() {
    let f = Fixture::new("audio_play");
    let out = f.engine.audio_out().clone();
    f.call(
        "node.batch",
        json!({"ops": [
            {"op": "create", "path": "/i", "kind": "audio.instrument", "params": {"decay": 0.02}},
            {"op": "create", "path": "/out", "kind": "out.audio"},
            {"op": "connect", "from": "/i", "to": "/out"},
            {"op": "create", "path": "/o", "kind": "audio.oscillator"},
        ]}),
    );
    let len = || out.pushed("").first().map_or(0, Vec::len);
    eventually("the bus to get blocks", || len() > 48_000 / 10);
    let before = len();
    assert!(out.pushed("")[0].iter().all(|s| *s == 0.0), "silent before the press");
    let r = f.call("audio.play", json!({"path": "/i"}));
    assert_eq!((r["played"].as_bool(), r["lane"].as_u64()), (Some(true), Some(0)), "{r}");
    eventually("the note", || out.pushed("")[0].iter().skip(before).any(|s| *s != 0.0));
    let start = out.pushed("")[0].iter().position(|s| *s != 0.0).unwrap_or(0);
    assert!(start >= before.saturating_sub(800), "the note starts after the press: {start} < {before}");
    // Another lane and a lane far out of range are fine.
    assert_eq!(f.call("audio.play", json!({"path": "/i", "lane": 3}))["played"], true);
    assert_eq!(f.call("audio.play", json!({"path": "/i", "lane": 1_000_000_000_u64}))["played"], true);
    // Let the notes die away (a short pluck rings about 0.1 s).
    std::thread::sleep(Duration::from_millis(600));

    // Paused: nothing is heard, and the press does not sound later.
    f.call("cook.play", json!({"playing": false}));
    let r = f.call("audio.play", json!({"path": "/i"}));
    assert_eq!(r["played"], false, "{r}");
    assert!(r["message"].as_str().unwrap_or_default().contains("paused"), "{r}");
    let resumed = len();
    f.call("cook.play", json!({"playing": true}));
    eventually("half a second after resuming", || len() > resumed + 24_000);
    let after = &out.pushed("")[0][resumed..];
    assert!(after.iter().all(|s| *s == 0.0), "a press while paused is dropped");

    // Wrong kind, no node, hostile args.
    assert!(f.fail("audio.play", json!({"path": "/o"})).contains("not an audio.instrument"));
    assert!(f.fail("audio.play", json!({"path": "/nope"})).contains("no node"));
    f.fail("audio.play", json!({}));
    f.fail("audio.play", json!({"path": "/i", "lane": -1}));
    f.fail("audio.play", json!({"path": "/i", "lane": 1e300}));
    f.fail("audio.play", json!({"path": "/i", "lane": "x", "extra": 1}));
}
