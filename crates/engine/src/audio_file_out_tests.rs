//! Sound recording through the commands: start, status, stop, a binding refused, and the file
//! finished when the app shuts down mid-recording.

use std::path::Path;
use std::time::Duration;

use penumbra_record::wav;
use serde_json::json;

use super::{Fixture, eventually};

/// The frequency of a tone, from its rising zero crossings.
fn frequency(c: &[f32], rate: f64) -> f64 {
    let rising: Vec<usize> = (1..c.len()).filter(|&k| c[k - 1] < 0.0 && c[k] >= 0.0).collect();
    let (first, last) = (rising[0], rising[rising.len() - 1]);
    (rising.len() - 1) as f64 * rate / (last - first) as f64
}

#[test]
fn commands_start_and_stop_a_sound_recording() {
    let f = Fixture::new("audio_file_out");
    f.call("node.create", json!({"path": "/osc", "kind": "audio.oscillator", "params": {"frequency": 1000}}));
    f.call("node.create", json!({"path": "/rec", "kind": "out.audio_file", "params": {"format": "float32"}}));
    f.call("node.connect", json!({"from": "/osc", "to": "/rec"}));
    assert!(f.fail("audio_file_out.start", json!({"path": "/osc"})).contains("not an out.audio_file"));
    eventually("the node to cook", || f.engine.record().all_sound().len() == 1);
    // Not a movie recording.
    assert_eq!(f.call("video_out.status", json!({}))["recordings"], json!([]));
    let s = f.call("audio_file_out.status", json!({"path": "/rec"}));
    assert_eq!(s["recording"], false, "{s}");
    let s = f.call("audio_file_out.start", json!({"path": "/rec"}));
    assert_eq!((s["recording"].clone(), s["opened"].clone()), (json!(true), json!(true)), "{s}");
    assert_eq!(s["node"], "/rec");
    assert!(s["file"].as_str().unwrap().ends_with("rec.wav"), "{s}");
    eventually("sound", || {
        f.call("audio_file_out.status", json!({"path": "/rec"}))["seconds"].as_f64() >= Some(0.3)
    });
    let s = f.call("audio_file_out.stop", json!({"path": "/rec"}));
    assert_eq!(s["recording"], false, "{s}");
    eventually("the file finished", || {
        f.call("audio_file_out.status", json!({}))["recordings"][0]["done"] == true
    });
    let s = f.call("audio_file_out.status", json!({}))["recordings"][0].clone();
    assert_eq!(
        (
            s["error"].clone(),
            s["format"].clone(),
            s["rate"].clone(),
            s["channels"].clone(),
            s["sources"].clone()
        ),
        (json!(null), json!("float32"), json!(48_000), json!(1), json!(null)),
        "{s}"
    );
    let w = wav::read(Path::new(s["file"].as_str().unwrap())).unwrap();
    assert_eq!(w.channels[0].len() as u64, s["samples"].as_u64().unwrap(), "{s}");
    assert!((frequency(&w.channels[0], 48_000.0) - 1000.0).abs() < 0.1);
    // Start and stop are edits of `record`, one undo step each.
    let info = f.call("node.info", json!({"path": "/rec"}));
    assert_eq!(info["params"][0], json!({"name": "record", "value": false}), "{info}");
    let history = f.call("history.list", json!({"limit": 2}));
    assert!(history.to_string().contains("start recording"), "{history}");
    assert!(history.to_string().contains("stop recording"), "{history}");
    // Bound to a channel, `record` is not the commands' to switch.
    f.call("node.create", json!({"path": "/on", "kind": "chan.constant"}));
    f.call("node.bind", json!({"param": "/rec.record", "from": "/on"}));
    assert!(f.fail("audio_file_out.start", json!({"path": "/rec"})).contains("follows a channel"));
    f.call("node.unbind", json!({"param": "/rec.record"}));

    // A second recording gets a new name; the shutdown finishes it.
    let s = f.call("audio_file_out.start", json!({"path": "/rec"}));
    assert!(s["file"].as_str().unwrap().ends_with("rec_2.wav"), "{s}");
    let again = f.call("audio_file_out.start", json!({"path": "/rec"}));
    assert_eq!(
        (again["recording"].clone(), again["file"].clone()),
        (json!(true), s["file"].clone()),
        "no-op"
    );
    std::thread::sleep(Duration::from_millis(300));
    f.engine.shutdown();
    let s = f.engine.record().all_sound()[0].1.clone();
    assert!(s.done && s.error.is_none() && s.samples > 0, "{s:?}");
    let w = wav::read(Path::new(&s.file)).unwrap();
    assert_eq!(w.channels[0].len() as u64, s.samples, "{s:?}");
}

#[test]
fn source_all_hears_the_out_audio_nodes() {
    let f = Fixture::new("audio_file_out_all");
    f.call("node.create", json!({"path": "/osc", "kind": "audio.oscillator", "params": {"frequency": 500}}));
    f.call("node.create", json!({"path": "/speakers", "kind": "out.audio", "params": {"reverb": 0}}));
    f.call("node.connect", json!({"from": "/osc", "to": "/speakers"}));
    f.call(
        "node.create",
        json!({"path": "/rec", "kind": "out.audio_file", "params": {"source": "all", "file": "takes/all"}}),
    );
    eventually("the node to cook", || f.engine.record().all_sound().len() == 1);
    f.call("audio_file_out.start", json!({"path": "/rec"}));
    eventually("sound", || {
        f.call("audio_file_out.status", json!({"path": "/rec"}))["seconds"].as_f64() >= Some(0.5)
    });
    f.call("audio_file_out.stop", json!({"path": "/rec"}));
    eventually("the file finished", || {
        f.call("audio_file_out.status", json!({"path": "/rec"}))["done"] == true
    });
    let s = f.call("audio_file_out.status", json!({"path": "/rec"}));
    assert_eq!(
        (s["error"].clone(), s["sources"].clone(), s["channels"].clone()),
        (json!(null), json!(1), json!(2))
    );
    let file = s["file"].as_str().unwrap();
    assert!(Path::new(file).ends_with("takes/all.wav"), "{s}");
    let w = wav::read(Path::new(file)).unwrap();
    assert!((frequency(&w.channels[0], 48_000.0) - 500.0).abs() < 0.1);
    assert_eq!(w.channels[0], w.channels[1], "a mono out.audio on both sides");
}
