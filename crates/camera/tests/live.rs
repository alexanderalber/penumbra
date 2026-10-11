//! Against a real camera: `cargo test -p penumbra-camera --test live -- --ignored --nocapture`.
//! Set `PN_CAMERA` to part of the camera's name and `PN_CAMERA_MODE` to a mode to pick them.

use std::time::{Duration, Instant};

use penumbra_camera::{Hub, Key, devices};

#[test]
#[ignore = "needs a camera"]
fn lists_and_reads_a_camera() {
    let list = devices().unwrap();
    for d in &list {
        println!("{} ({})", d.name, d.id);
        if let Some(e) = &d.error {
            println!("  error: {e}");
        }
        for m in &d.modes {
            println!("  {}{}", m.label, if m.readable { "" } else { "  (not read)" });
        }
    }
    let key = Key::new(
        &std::env::var("PN_CAMERA").unwrap_or_default(),
        &std::env::var("PN_CAMERA_MODE").unwrap_or_default(),
    );
    let hub = Hub::new();
    hub.set_wanted(std::slice::from_ref(&key));
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(6) {
        std::thread::sleep(Duration::from_secs(1));
        println!("{:?}", hub.stream_status(&key));
    }
    let f = hub.latest(&key).expect("no frame arrived");
    let mean = f.y.iter().map(|v| f64::from(*v)).sum::<f64>() / f.y.len() as f64;
    println!(
        "last frame {}x{} {:?} #{} matrix {:?} full {} mean luma {mean:.1}",
        f.width, f.height, f.format, f.sequence, f.matrix, f.full_range
    );
    hub.shutdown();
}
