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
            {"op": "create", "path": "/a", "kind": "chan.beat_analysis"},
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
            {"op": "create", "path": "/a", "kind": "chan.beat_analysis"},
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
        "/a": (kind: "chan.beat_analysis", inputs: {"audio": "mic"}),
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
    // `rms`, since `level` reads 0 when the last block is the quiet tail between two kicks.
    eventually("the analysis to hear it", || {
        channel(&f.call("chan.read", json!({"path": "/a"})), "rms") > 0.0
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
            {"op": "create", "path": "/out", "kind": "out.audio", "params": {"reverb": 0.0}},
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
            // Dry: the test listens for the end of each note.
            {"op": "create", "path": "/out", "kind": "out.audio", "params": {"reverb": 0.0}},
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

/// `audio.file` through `out.audio`: the file is heard, `audio.file_info` says what it is,
/// `audio.play` starts it over (a one-shot with loop off), hostile args are errors.
#[test]
fn an_audio_file_plays_and_restarts_on_a_press() {
    use penumbra_audio::file::{WavFormat, wav_bytes};
    let f = Fixture::new("audio_file");
    let out = f.engine.audio_out().clone();
    let dir = std::env::temp_dir().join(format!("penumbra-engine-audio-file-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // 0.2 s of a 440 Hz tone at 44.1 kHz.
    let tone: Vec<f64> =
        (0..8820).map(|i| 0.5 * (i as f64 * std::f64::consts::TAU * 440.0 / 44_100.0).sin()).collect();
    let path = dir.join("tone.wav");
    std::fs::write(&path, wav_bytes(44_100, 1, WavFormat::Pcm16, &tone)).unwrap();
    f.call(
        "node.batch",
        json!({"ops": [
            {"op": "create", "path": "/s", "kind": "audio.file",
             "params": {"file": path.to_string_lossy(), "loop": false}},
            {"op": "create", "path": "/out", "kind": "out.audio", "params": {"reverb": 0.0}},
            {"op": "connect", "from": "/s", "to": "/out"},
            {"op": "create", "path": "/o", "kind": "audio.oscillator"},
        ]}),
    );
    let len = || out.pushed("").first().map_or(0, Vec::len);
    eventually("the tone", || out.pushed("").first().is_some_and(|b| b.iter().any(|s| *s != 0.0)));
    let info = f.call("audio.file_info", json!({"path": "/s"}));
    assert_eq!((info["codec"].as_str(), info["rate"].as_u64()), (Some("pcm_s16le"), Some(44_100)), "{info}");
    assert_eq!((info["channels"].as_u64(), info["in_memory"].as_bool()), (Some(1), Some(true)), "{info}");
    assert!((info["duration"].as_f64().unwrap_or(0.0) - 0.2).abs() < 1e-9, "{info}");
    // Played once: silent after its 0.2 s.
    eventually("the end of the tone", || len() > 48_000 / 2);
    let quiet = len();
    std::thread::sleep(Duration::from_millis(100));
    assert!(out.pushed("")[0][quiet..].iter().all(|s| *s == 0.0), "played once");
    let r = f.call("audio.play", json!({"path": "/s"}));
    assert_eq!(r["played"], true, "{r}");
    eventually("the tone again", || out.pushed("")[0].iter().skip(quiet).any(|s| *s != 0.0));
    assert_eq!(f.call("audio.file_info", json!({"path": "/s"}))["restarts"], 1);
    // A seek into the tone after its end plays its last part again, without a restart.
    eventually("the end of the tone again", || {
        f.call("audio.file_info", json!({"path": "/s"}))["position"].as_f64().is_some_and(|p| p >= 0.2)
    });
    std::thread::sleep(Duration::from_millis(100));
    let quiet = len();
    let r = f.call("audio.seek", json!({"path": "/s", "seconds": 0.1}));
    assert_eq!(r["seconds"], 0.1, "{r}");
    eventually("the tone after the seek", || out.pushed("")[0].iter().skip(quiet).any(|s| *s != 0.0));
    assert_eq!(f.call("audio.file_info", json!({"path": "/s"}))["restarts"], 1);
    assert!(f.fail("audio.seek", json!({"path": "/o", "seconds": 1.0})).contains("for audio.file nodes"));
    assert!(f.fail("audio.seek", json!({"path": "/s", "seconds": -1.0})).contains("0 or more"));
    f.fail("audio.seek", json!({"path": "/s"}));
    f.call(
        "node.batch",
        json!({"ops": [
            {"op": "create", "path": "/k", "kind": "chan.constant"},
            {"op": "bind", "param": "/s.position", "from": "/k", "channel": "chan1"},
        ]}),
    );
    assert!(f.fail("audio.seek", json!({"path": "/s", "seconds": 0.0})).contains("bound"));
    let info = f.call("node.info", json!({"path": "/s"}));
    let speed = info["params"].as_array().unwrap().iter().find(|p| p["name"] == "speed").unwrap().clone();
    assert_eq!(speed["why"], "Does nothing while position is bound", "{info}");

    assert!(f.fail("audio.file_info", json!({"path": "/o"})).contains("not an audio.file"));
    assert!(f.fail("audio.play", json!({"path": "/o"})).contains("not an audio.instrument or audio.file"));
    f.fail("audio.file_info", json!({}));
    f.fail("audio.file_info", json!({"path": "/s", "extra": 1}));
    let _ = std::fs::remove_dir_all(&dir);
}

/// `chan.beat_grid` on an `audio.file`: `beats.analyze` finds the beats (waiting by default),
/// `beats.status` and `beats.grid` report them, the node's channels read them, `beats.cancel`
/// has nothing left to stop; files that are not there and nodes of other kinds are errors.
#[test]
fn beats_of_a_sound_file_through_the_commands() {
    use penumbra_audio::file::{WavFormat, wav_bytes};
    let f = Fixture::new("beat_grid");
    let dir = std::env::temp_dir().join(format!("penumbra-engine-beat-grid-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // 12 s of clicks at 120 beats per minute.
    let rate = 44_100usize;
    let mut clicks = vec![0.0f64; 12 * rate];
    for beat in 0..24 {
        for i in 0..400 {
            clicks[beat * rate / 2 + i] = 0.6 * (-(i as f64) / 60.0).exp() * (i as f64 * 0.35).sin();
        }
    }
    let path = dir.join("clicks.wav");
    std::fs::write(&path, wav_bytes(44_100, 1, WavFormat::Float32, &clicks)).unwrap();
    f.call(
        "node.batch",
        json!({"ops": [
            {"op": "create", "path": "/s", "kind": "audio.file", "params": {"file": path.to_string_lossy()}},
            {"op": "create", "path": "/g", "kind": "chan.beat_grid"},
            {"op": "connect", "from": "/s", "to": "/g"},
            {"op": "create", "path": "/o", "kind": "audio.oscillator"},
        ]}),
    );
    let r = f.call("beats.analyze", json!({"path": "/g"}));
    assert_eq!((r["complete"].as_bool(), r["running"].as_bool()), (Some(true), Some(false)), "{r}");
    assert!((20..=26).contains(&r["beats"].as_u64().unwrap_or(0)), "{r}");
    assert!((r["bpm"].as_f64().unwrap_or(0.0) - 120.0).abs() < 2.0, "{r}");
    let status = f.call("beats.status", json!({}));
    assert_eq!(status["analyses"].as_array().map(Vec::len), Some(1), "{status}");
    assert_eq!(f.call("beats.status", json!({"path": "/s"}))["beats"], r["beats"]);
    let grid = f.call("beats.grid", json!({"path": "/s", "from": 0.9, "to": 2.1}));
    let beats: Vec<f64> = grid["beats"].as_array().unwrap().iter().filter_map(Json::as_f64).collect();
    assert_eq!(beats.len(), 3, "{grid}");
    assert!(beats.iter().zip([1.0, 1.5, 2.0]).all(|(b, w)| (b - w).abs() < 0.03), "{grid}");
    // The sections are found before beats.analyze answers: the clicks are one section.
    let x = &r["sections"];
    assert_eq!((x["ready"].as_bool(), x["running"].as_bool()), (Some(true), Some(false)), "{r}");
    assert!(x["sections"].as_u64().unwrap_or(0) >= 1 && x["tracks"].as_u64() == Some(1), "{r}");
    let list = grid["sections"].as_array().unwrap();
    assert!(!list.is_empty() && list[0]["letter"] == "A", "{grid}");
    assert!(list[0]["start"].as_f64().unwrap() <= 2.1 && list[0]["end"].as_f64().unwrap() >= 0.9, "{grid}");
    let outside = f.call("beats.grid", json!({"path": "/s", "from": 100.0}));
    assert_eq!(outside["sections"].as_array().map(Vec::len), Some(0), "{outside}");

    f.call("node.watch", json!({"path": "/g"}));
    eventually("the grid's channels", || {
        let read = f.call("chan.read", json!({"path": "/g"}));
        channel(&read, "ready") == 1.0
            && (channel(&read, "bpm") - 120.0).abs() < 2.0
            && channel(&read, "sections_ready") == 1.0
    });
    let r = f.call("beats.cancel", json!({"file": path.to_string_lossy()}));
    assert_eq!(r["cancelled"], false, "{r}");

    assert!(f.fail("beats.analyze", json!({"path": "/o"})).contains("not an audio.file"));
    assert!(f.fail("beats.analyze", json!({"file": "nothing.wav"})).contains("not a file"));
    assert!(f.fail("beats.grid", json!({"file": "nothing.wav"})).contains("no beat analysis"));
    f.fail("beats.analyze", json!({}));
    f.fail("beats.analyze", json!({"path": "/s", "file": "x.wav"}));
    f.fail("beats.status", json!({"path": "/s", "extra": 1}));
    let _ = std::fs::remove_dir_all(&dir);
}

/// `beats.cues` writes a `chan.sequence`'s steps from the sections of the example track and binds
/// its clock to the grid's seconds, as one history entry that one undo takes back; before the
/// sections, at a node of another kind, at no node and with arguments it does not know it is an
/// error and changes nothing.
#[test]
fn beats_cues_writes_a_cue_list_as_one_edit() {
    let f = Fixture::new("beat_cues");
    let music = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/beat-grid/music.wav");
    f.call(
        "node.batch",
        json!({"ops": [
            {"op": "create", "path": "/p", "kind": "audio.file", "params": {"file": music.to_string_lossy()}},
            {"op": "create", "path": "/g", "kind": "chan.beat_grid"},
            {"op": "connect", "from": "/p", "to": "/g"},
            {"op": "create", "path": "/seq", "kind": "chan.sequence", "params": {"steps": "x 5", "lead_in": 2.0}},
            {"op": "create", "path": "/n", "kind": "chan.constant"},
        ]}),
    );
    let cues = |extra: Json| {
        let mut a = json!({"path": "/g", "sequence": "/seq"});
        if let (Some(o), Some(x)) = (a.as_object_mut(), extra.as_object()) {
            o.extend(x.clone());
        }
        a
    };
    // No analysis yet, or one still running: no sections.
    let e = f.fail("beats.cues", cues(json!({})));
    assert!(e.contains("no beat analysis"), "{e}");
    f.call("beats.analyze", json!({"path": "/g", "wait": false}));
    let e = f.fail("beats.cues", cues(json!({})));
    assert!(e.contains("sections") || e.contains("no beat analysis"), "{e}");
    let status = f.call("beats.analyze", json!({"path": "/g"}));
    let sections = status["sections"]["sections"].as_u64().unwrap();
    assert!(sections >= 2, "{status}");

    for (args, says) in [
        (json!({"path": "/g", "sequence": "/nothing"}), "no node at /nothing"),
        (json!({"path": "/g", "sequence": "/n"}), "not a chan.sequence"),
        (json!({"path": "/seq", "sequence": "/seq"}), "not a chan.beat_grid"),
        (json!({"path": "/p", "sequence": "/seq"}), "not a chan.beat_grid"),
        (cues(json!({"level": "bar"})), "level"),
        (cues(json!({"pieces": "colour"})), "pieces"),
        (cues(json!({"transition": "flash"})), "transition"),
        (cues(json!({"extra": 1})), "extra"),
        (json!({"sequence": "/seq"}), "path"),
    ] {
        let e = f.fail("beats.cues", args.clone());
        assert!(e.contains(says), "{args}: {e}");
    }
    let before = f.call("node.info", json!({"path": "/seq"}));
    let param = |info: &Json, name: &str| {
        info["params"].as_array().unwrap().iter().find(|p| p["name"] == name).unwrap()["value"].clone()
    };
    assert_eq!(param(&before, "steps"), "x 5");

    let r = f.call("beats.cues", cues(json!({})));
    assert_eq!(r["history"], "new", "{r}");
    assert_eq!(r["steps"].as_u64(), Some(sections), "{r}");
    let info = f.call("node.info", json!({"path": "/seq"}));
    let steps = param(&info, "steps");
    assert!(steps.as_str().unwrap().starts_with("# One step per section of music.wav"), "{steps}");
    assert_eq!(param(&info, "lead_in"), 0.0);
    assert_eq!(param(&info, "names"), r["names"]);
    assert_eq!(info["binds"], json!([{"param": "clock", "from": "/g:seconds"}]), "{info}");
    // The cue list plays the music's sections: the show's step is the grid's section.
    f.call("node.watch", json!({"path": "/seq"}));
    eventually("the show at the grid's section", || {
        let g = f.call("chan.read", json!({"path": "/g"}));
        let s = f.call("chan.read", json!({"path": "/seq"}));
        channel(&g, "sections_ready") == 1.0 && channel(&s, "step") == channel(&g, "section_index")
    });
    // One undo takes all of it back.
    f.call("history.undo", json!({}));
    let undone = f.call("node.info", json!({"path": "/seq"}));
    assert_eq!(param(&undone, "steps"), "x 5");
    assert_eq!(param(&undone, "lead_in"), 2.0);
    assert_eq!(undone["binds"], json!([]), "{undone}");
    // Per track, by letter, cut.
    let r = f.call("beats.cues", cues(json!({"level": "track", "pieces": "letter", "transition": "cut"})));
    assert_eq!(r["steps"].as_u64(), status["sections"]["tracks"].as_u64(), "{r}");
    assert_eq!(r["names"], "A");
}

/// A show whose `clock` follows the music (`beats.cues`) comes back where the music was after a
/// crash restart in show mode: `audio.file` keeps its playhead, the grid's `seconds` follow it,
/// and the cue list stands at the section the music is in, not at its first step.
#[test]
fn a_cue_list_on_the_music_comes_back_with_the_music_after_a_crash_restart() {
    let f = Fixture::new("beat_cues_restart");
    let data = f._dir.join("data");
    let show_file = data.join(crate::show::FILE);
    let kept = data.join(crate::show_values::FILE);
    f.engine.set_show_file(Some(show_file.clone()));
    let music = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/beat-grid/music.wav");
    f.call(
        "node.batch",
        json!({"ops": [
            {"op": "create", "path": "/p", "kind": "audio.file", "params": {"file": music.to_string_lossy()}},
            {"op": "create", "path": "/g", "kind": "chan.beat_grid"},
            {"op": "connect", "from": "/p", "to": "/g"},
            {"op": "create", "path": "/seq", "kind": "chan.sequence"},
        ]}),
    );
    f.call("beats.analyze", json!({"path": "/g"}));
    f.call("beats.cues", json!({"path": "/g", "sequence": "/seq"}));
    f.call("node.watch", json!({"path": "/seq"}));
    let read =
        |r: &crate::Registry, path: &str| r.dispatch("chan.read", &json!({"path": path}))["result"].clone();
    eventually("the music playing", || {
        f.registry.dispatch("audio.file_info", &json!({"path": "/p"}))["ok"] == true
    });
    // Into the breakdown, the third section, 30.98 to 46.46 s.
    f.call("audio.seek", json!({"path": "/p", "seconds": 38.0}));
    eventually("the show in the breakdown", || channel(&read(&f.registry, "/seq"), "step") == 2.0);
    eventually("autosave", || f.file().contains("g:seconds"));
    f.call("show.lock", json!({}));
    let kept_at = || -> f64 {
        let k: Json = std::fs::read_to_string(&kept)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let place: Json = k["places"]["/p"]["place"]
            .as_str()
            .and_then(|p| serde_json::from_str(p).ok())
            .unwrap_or_default();
        place["seconds"].as_f64().or_else(|| k["places"]["/p"]["place"]["seconds"].as_f64()).unwrap_or(-1.0)
    };
    eventually("the playhead kept", || kept_at() >= 38.0);
    let at = kept_at();
    assert!(at < 45.0, "{at}");
    // The clock-driven show keeps no place of its own: the music is its place.
    let text = std::fs::read_to_string(&kept).unwrap();
    assert!(!text.contains("\"/seq\""), "{text}");
    f.engine.shutdown();

    let types = std::sync::Arc::new(penumbra_ops::builtin_types().unwrap());
    let session =
        penumbra_project::Session::open(f._dir.path(), types, &penumbra_project::content::Content::default())
            .unwrap();
    let e = crate::Engine::start(std::sync::Arc::new(session)).unwrap();
    assert!(e.set_show_file(Some(show_file.clone())));
    let r = crate::engine_registry(&e);
    r.dispatch("node.watch", &json!({"path": "/seq"}));
    let position = || r.dispatch("audio.file_info", &json!({"path": "/p"}))["result"]["position"].as_f64();
    eventually("the music where it was", || position().is_some_and(|p| p >= at && p < at + 5.0));
    eventually("the show at the music's section", || {
        let (s, g) = (read(&r, "/seq"), read(&r, "/g"));
        s["channels"].is_array() && channel(&s, "step") == 2.0 && channel(&g, "section_index") == 2.0
    });
    e.shutdown();
}
