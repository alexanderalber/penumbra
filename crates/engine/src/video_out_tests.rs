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

/// A clip of `n` flat frames getting lighter, 64 x 48 ProRes 422 at 20 frames per second.
fn steps_clip(dir: &std::path::Path, n: u32) -> std::path::PathBuf {
    use penumbra_record::{Codec, Fps, H264Preset, Settings, Sink};
    let settings = Settings {
        path: dir.join("steps.mov"),
        codec: Codec::Prores422,
        width: 64,
        height: 48,
        fps: Fps { num: 20, den: 1 },
        threads: 2,
        queue: 1,
        h264: H264Preset::Balanced,
    };
    let mut sink = Sink::create(&settings, |_| {}).unwrap();
    let layout = sink.layout();
    let [_, ou, _, oa] = layout.offsets();
    for k in 0..n {
        let mut b = vec![0u8; layout.bytes()];
        let y = ((40 + 4 * k) * 4) as u16;
        for s in b[..ou].as_chunks_mut::<2>().0 {
            s.copy_from_slice(&y.to_le_bytes());
        }
        for s in b[ou..oa].as_chunks_mut::<2>().0 {
            s.copy_from_slice(&512u16.to_le_bytes());
        }
        sink.push(&b).unwrap();
    }
    let path = sink.path().to_path_buf();
    sink.finish().unwrap();
    path
}

#[test]
fn video_loop_makes_a_loop_in_the_background() {
    let f = Fixture::new("video_loop");
    let dir = f.engine.session().dir().to_path_buf();
    std::fs::create_dir_all(&dir).unwrap();
    // Bad requests fail at once.
    assert!(f.fail("video.loop", json!({"file": "missing.mov", "method": "fade"})).contains("no file"));
    let clip = steps_clip(&dir, 30);
    assert!(f.fail("video.loop", json!({"file": "steps.mov", "method": "spin"})).contains("unknown method"));
    assert!(
        f.fail("video.loop", json!({"file": "steps.mov", "method": "fade", "codec": "gif"}))
            .contains("unknown codec")
    );
    assert!(f.fail("video.loop", json!({"file": "steps.mov", "method": "cut"})).contains("min_seconds"));
    assert!(f.fail("video.loop_status", json!({"file": "steps.mov"})).contains("no loop"));
    // A fade of 0.25 s (5 frames), by a path relative to the project.
    let s = f.call("video.loop", json!({"file": "steps.mov", "method": "fade", "fade_seconds": 0.25}));
    assert_eq!((s["method"].clone(), s["codec"].clone()), (json!("fade"), json!("prores_422")), "{s}");
    assert_eq!(s["frames_in"], 30, "{s}");
    let out = std::path::PathBuf::from(s["out"].as_str().unwrap());
    assert_eq!(out, dir.join("steps_loop.mov"));
    eventually("the loop", || f.call("video.loop_status", json!({"file": "steps.mov"}))["done"] == true);
    let s = f.call("video.loop_status", json!({"file": clip.display().to_string()}));
    assert_eq!(
        (s["error"].clone(), s["frames_out"].clone(), s["fade_frames"].clone()),
        (json!(null), json!(25), json!(5)),
        "{s}"
    );
    let d = penumbra_video::open(&out, &penumbra_video::Limits::default()).unwrap();
    assert_eq!(d.info().frames, 25);
    // The list has it too, and the loop's own name finds it.
    assert_eq!(f.call("video.loop_status", json!({}))["loops"].as_array().unwrap().len(), 1);
    assert_eq!(f.call("video.loop_status", json!({"file": "steps_loop.mov"}))["out"], s["out"]);
    // A pingpong to a file named, in H.264.
    let s = f.call(
        "video.loop",
        json!({"file": "steps.mov", "method": "pingpong", "out": "loops/pp", "codec": "h264"}),
    );
    assert!(s["out"].as_str().unwrap().ends_with("pp.mp4"), "{s}");
    eventually("the pingpong", || {
        f.call("video.loop_status", json!({"file": "loops/pp.mp4"}))["done"] == true
    });
    let s = f.call("video.loop_status", json!({"file": "loops/pp.mp4"}));
    assert_eq!((s["error"].clone(), s["frames_out"].clone()), (json!(null), json!(58)), "{s}");
}

#[test]
fn a_recording_with_loop_set_is_looped_when_it_stops() {
    let Some(f) = with_gpu("video_out_loop") else { return };
    f.call("node.create", json!({"path": "/n", "kind": "tex.noise", "params": {"width": 64, "height": 32}}));
    f.call(
        "node.create",
        json!({"path": "/rec", "kind": "out.video", "params": {"codec": "prores_422", "loop": "fade", "loop_fade": 0.1}}),
    );
    f.call("node.connect", json!({"from": "/n", "to": "/rec"}));
    eventually("the node to cook", || f.engine.record().all().len() == 1);
    f.call("video_out.start", json!({"path": "/rec"}));
    eventually("frames", || {
        f.call("video_out.status", json!({"path": "/rec"}))["frames"].as_u64() >= Some(20)
    });
    f.call("video_out.stop", json!({"path": "/rec"}));
    eventually("the loop", || f.call("video_out.status", json!({"path": "/rec"}))["loop"]["done"] == true);
    let s = f.call("video_out.status", json!({"path": "/rec"}));
    let l = &s["loop"];
    assert_eq!((l["error"].clone(), l["method"].clone()), (json!(null), json!("fade")), "{s}");
    assert!(l["out"].as_str().unwrap().ends_with("rec_loop.mov"), "{s}");
    assert!(l["frames_out"].as_u64().unwrap() >= 14, "{s}");
}
