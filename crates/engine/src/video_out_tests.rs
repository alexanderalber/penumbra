//! Recording through the commands, on the software adapter: start, status, stop, and the file
//! finished when the app shuts down mid-recording.

use std::time::Duration;

use serde_json::json;

use super::{Fixture, eventually};

fn with_gpu(name: &str) -> Option<Fixture> {
    let gpu = penumbra_gpu::Gpu::headless(penumbra_gpu::AdapterChoice::Software).ok()?;
    let f = Fixture::new(name);
    f.engine.attach_gpu(gpu).unwrap();
    Some(f)
}

#[test]
fn commands_start_and_stop_a_recording() {
    let Some(f) = with_gpu("video_out") else { return };
    f.call("node.create", json!({"path": "/n", "kind": "tex.noise", "params": {"width": 64, "height": 32}}));
    f.call("node.create", json!({"path": "/rec", "kind": "out.video", "params": {"codec": "prores_422"}}));
    f.call("node.connect", json!({"from": "/n", "to": "/rec"}));
    assert!(f.fail("video_out.start", json!({"path": "/n"})).contains("not an out.video"));
    eventually("the node to cook", || f.engine.record().all().len() == 1);
    let s = f.call("video_out.status", json!({"path": "/rec"}));
    assert_eq!(s["recording"], false, "{s}");
    let s = f.call("video_out.start", json!({"path": "/rec"}));
    assert_eq!(s["recording"], true, "{s}");
    assert_eq!(s["node"], "/rec");
    assert!(s["file"].as_str().unwrap().ends_with("rec.mov"), "{s}");
    eventually("frames", || {
        f.call("video_out.status", json!({"path": "/rec"}))["frames"].as_u64() >= Some(10)
    });
    let s = f.call("video_out.stop", json!({"path": "/rec"}));
    assert_eq!(s["recording"], false, "{s}");
    eventually("the file finished", || {
        f.call("video_out.status", json!({}))["recordings"][0]["done"] == true
    });
    let s = f.call("video_out.status", json!({}))["recordings"][0].clone();
    assert_eq!(
        (s["error"].clone(), s["width"].clone(), s["height"].clone()),
        (json!(null), json!(64), json!(32))
    );
    let file = std::path::PathBuf::from(s["file"].as_str().unwrap());
    let d = penumbra_video::open(&file, &penumbra_video::Limits::default()).unwrap();
    // Frames the show skipped make the frame before last longer; a reader counts by time.
    let frames =
        s["frames"].as_u64().unwrap() + s["skipped"].as_u64().unwrap() + s["dropped"].as_u64().unwrap();
    assert_eq!(d.info().frames as u64, frames, "{s}");
    // Start and stop are edits of `record`, one undo step each.
    let info = f.call("node.info", json!({"path": "/rec"}));
    assert_eq!(info["params"][0], json!({"name": "record", "value": false}), "{info}");
    let history = f.call("history.list", json!({"limit": 2}));
    assert!(history.to_string().contains("start recording"), "{history}");
    assert!(history.to_string().contains("stop recording"), "{history}");
    // Bound to a channel, `record` is not the commands' to switch.
    f.call("node.create", json!({"path": "/on", "kind": "chan.constant"}));
    f.call("node.bind", json!({"param": "/rec.record", "from": "/on"}));
    assert!(f.fail("video_out.start", json!({"path": "/rec"})).contains("follows a channel"));
    f.call("node.unbind", json!({"param": "/rec.record"}));

    // A second recording gets a new name; the shutdown finishes it.
    let s = f.call("video_out.start", json!({"path": "/rec"}));
    assert!(s["file"].as_str().unwrap().ends_with("rec_2.mov"), "{s}");
    let again = f.call("video_out.start", json!({"path": "/rec"}));
    assert_eq!(
        (again["recording"].clone(), again["file"].clone()),
        (json!(true), s["file"].clone()),
        "no-op"
    );
    std::thread::sleep(Duration::from_millis(300));
    f.engine.shutdown();
    let s = f.engine.record().all()[0].1.clone();
    assert!(s.done && s.error.is_none() && s.frames > 0, "{s:?}");
    let d = penumbra_video::open(std::path::Path::new(&s.file), &penumbra_video::Limits::default()).unwrap();
    assert_eq!(d.info().frames as u64, s.frames + s.skipped + s.dropped, "{s:?}");
}
