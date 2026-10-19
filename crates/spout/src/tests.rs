//! Sending to and receiving from a plain D3D11 device, which stands in for another Spout
//! program: it only knows the shared handle from the directory, as another process would.
//! On WARP by default; `cargo test -p penumbra-spout -- --ignored` repeats them on the GPU.

use std::sync::OnceLock;
use std::time::Duration;

use penumbra_gpu::{AdapterChoice, Gpu, Tex};

use crate::d3d::testing::Other;
use crate::directory::{self, Registration};
use crate::protocol::Info;
use crate::{Alpha, Format, Received, Receiver, Sender, Sent};

fn gpu(choice: AdapterChoice) -> Gpu {
    static SOFT: OnceLock<Gpu> = OnceLock::new();
    static HARD: OnceLock<Gpu> = OnceLock::new();
    let cell = if choice == AdapterChoice::Software { &SOFT } else { &HARD };
    cell.get_or_init(|| Gpu::headless(choice).unwrap()).clone()
}

/// Waits for everything on the queue, the D3D11 copies included: an empty wgpu submission
/// after them finishes after them.
fn finish(gpu: &Gpu) {
    let encoder = gpu.device().create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    gpu.submit([encoder.finish()]);
    gpu.settle(Duration::from_secs(10));
}

/// A unique name per test and process, so parallel test runs do not meet.
fn name(test: &str) -> String {
    format!("penumbra test {test} {}", std::process::id())
}

/// A texture of one colour (linear, premultiplied).
fn solid(gpu: &Gpu, w: u32, h: u32, c: [f32; 4]) -> Tex {
    let mut bytes = Vec::new();
    for _ in 0..w * h {
        for v in c {
            bytes.extend_from_slice(&half::f16::from_f32(v).to_le_bytes());
        }
    }
    gpu.upload_rgba16f(w, h, &mut bytes, "solid").unwrap()
}

fn srgb8(linear: f32) -> u8 {
    let v = if linear <= 0.0031308 { linear * 12.92 } else { 1.055 * linear.powf(1.0 / 2.4) - 0.055 };
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn close(a: u8, b: u8) -> bool {
    a.abs_diff(b) <= 1
}

fn sends_to_another_device(choice: AdapterChoice) {
    let gpu = gpu(choice);
    let mut sender = Sender::new(&gpu, &name(&format!("send {choice:?}"))).unwrap();
    // Straight (0.2, 0.5, 1.0) at alpha 0.5.
    let tex = solid(&gpu, 6, 4, [0.1, 0.25, 0.5, 0.5]);
    assert_eq!(sender.send(&gpu, &tex, Format::Bgra8, Alpha::Straight).unwrap(), Sent::Frame);
    finish(&gpu);
    let info = directory::info(sender.name()).unwrap();
    assert_eq!((info.width, info.height, info.format), (6, 4, 87));
    assert!(info.program().to_lowercase().ends_with(".exe"), "{}", info.program());
    let other = Other::new(choice == AdapterChoice::Software);
    let (desc, px) = other.read(info.handle);
    assert_eq!((desc.Width, desc.Height), (6, 4));
    let expect = [srgb8(1.0), srgb8(0.5), srgb8(0.2), 128];
    for p in px.as_chunks::<4>().0.iter() {
        assert!(p.iter().zip(expect).all(|(a, b)| close(*a, b)), "BGRA {p:?}, expected {expect:?}");
    }

    // Another size: a new texture under a new handle; the old one is gone.
    let tex = solid(&gpu, 3, 5, [0.0, 0.0, 0.0, 0.0]);
    sender.send(&gpu, &tex, Format::Bgra8, Alpha::Straight).unwrap();
    finish(&gpu);
    let moved = directory::info(sender.name()).unwrap();
    assert_eq!((moved.width, moved.height), (3, 5));
    assert_ne!(moved.handle, info.handle);
    let (_, px) = other.read(moved.handle);
    assert!(px.iter().all(|b| *b == 0));

    // Opaque: over black, alpha 1.
    let tex = solid(&gpu, 2, 2, [0.1, 0.25, 0.5, 0.5]);
    sender.send(&gpu, &tex, Format::Bgra8, Alpha::Opaque).unwrap();
    finish(&gpu);
    let (_, px) = other.read(directory::info(sender.name()).unwrap().handle);
    let expect = [srgb8(0.5), srgb8(0.25), srgb8(0.1), 255];
    assert!(px.as_chunks::<4>().0.iter().all(|p| p.iter().zip(expect).all(|(a, b)| close(*a, b))), "{px:?}");
}

fn receives_from_another_device(choice: AdapterChoice) {
    let gpu = gpu(choice);
    let other = Other::new(choice == AdapterChoice::Software);
    let (w, h) = (5, 3);
    // BGRA: straight sRGB (124, 188, 255) = linear (0.2, 0.5, 1.0), alpha 128.
    let px: Vec<u8> = (0..w * h).flat_map(|_| [255u8, 188, 124, 128]).collect();
    let (_texture, handle) = other.shared_bgra(w, h, &px);
    let sender_name = name(&format!("receive {choice:?}"));
    let info = Info { handle, width: w, height: h, format: 87, ..Info::default() };
    let registration = Registration::new(&sender_name, &info).unwrap();
    assert!(crate::senders().iter().any(|l| l.name == sender_name && l.width == w));

    let mut receiver = Receiver::new(&gpu).unwrap();
    let Received::Frame(tex) = receiver.receive(&gpu, &sender_name, Alpha::Straight, false).unwrap() else {
        panic!("no frame");
    };
    finish(&gpu);
    assert_eq!(tex.size(), (w, h));
    let got = gpu.read(&tex).unwrap();
    let a = 128.0 / 255.0;
    let lin = |v: f32| ((v + 0.055) / 1.055).powf(2.4);
    let expect = [lin(124.0 / 255.0) * a, lin(188.0 / 255.0) * a, 1.0 * a, a];
    for p in &got.data {
        assert!(p.iter().zip(expect).all(|(x, y)| (x - y).abs() < 4e-3), "{p:?}, expected {expect:?}");
    }
    let connected = receiver.connected().unwrap();
    assert_eq!((connected.width, connected.height), (w, h));
    // The registration keeps a frame counter, as a sender does: a frame not counted is not
    // copied again, a counted one is.
    assert!(connected.counts_frames);
    assert!(matches!(
        receiver.receive(&gpu, &sender_name, Alpha::Straight, false).unwrap(),
        Received::Unchanged
    ));
    registration.frame_sent();
    assert!(matches!(
        receiver.receive(&gpu, &sender_name, Alpha::Straight, false).unwrap(),
        Received::Frame(_)
    ));
    assert!(matches!(
        receiver.receive(&gpu, &sender_name, Alpha::Straight, true).unwrap(),
        Received::Frame(_)
    ));
    assert!(receiver.receive(&gpu, "penumbra test nobody", Alpha::Straight, false).is_err());
}

#[test]
fn sends_to_another_device_on_warp() {
    sends_to_another_device(AdapterChoice::Software);
}

#[test]
fn receives_from_another_device_on_warp() {
    receives_from_another_device(AdapterChoice::Software);
}

#[test]
#[ignore]
fn sends_to_another_device_on_the_gpu() {
    sends_to_another_device(AdapterChoice::Hardware);
}

#[test]
#[ignore]
fn receives_from_another_device_on_the_gpu() {
    receives_from_another_device(AdapterChoice::Hardware);
}

#[test]
fn a_second_sender_of_a_name_gets_a_number_and_leaves_the_list() {
    let base = name("names");
    let a = Registration::new(&base, &Info::default()).unwrap();
    let b = Registration::new(&base, &Info::default()).unwrap();
    assert_eq!(a.name(), base);
    assert_eq!(b.name(), format!("{base}_1"));
    drop(b);
    let listed: Vec<String> = directory::senders().into_iter().map(|l| l.name).collect();
    assert!(listed.contains(&base));
    assert!(!listed.contains(&format!("{base}_1")));
    drop(a);
    assert!(!directory::senders().iter().any(|l| l.name.starts_with(&base)));
}

/// The real case: another process opens the sender by its name and reads the frame. Runs this
/// test binary again as the other process ([`child_reads_the_sender`]). On the GPU only: WARP
/// does not share textures between processes.
#[test]
#[ignore]
fn another_process_reads_the_frame_on_the_gpu() {
    let gpu = gpu(AdapterChoice::Hardware);
    let mut sender = Sender::new(&gpu, &name("process")).unwrap();
    let tex = solid(&gpu, 16, 9, [0.1, 0.25, 0.5, 0.5]);
    sender.send(&gpu, &tex, Format::Bgra8, Alpha::Straight).unwrap();
    finish(&gpu);
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "tests::child_reads_the_sender", "--nocapture"])
        .env("PENUMBRA_SPOUT_CHILD", sender.name())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && text.contains("1 passed"),
        "{text}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The other process of [`another_process_reads_the_frame_on_the_gpu`]; does nothing alone.
#[test]
#[ignore]
fn child_reads_the_sender() {
    let Ok(name) = std::env::var("PENUMBRA_SPOUT_CHILD") else { return };
    let info = directory::info(&name).unwrap();
    assert!(crate::senders().iter().any(|l| l.name == name && l.width == 16 && l.height == 9));
    let (_, px) = Other::new(false).read(info.handle);
    let expect = [srgb8(1.0), srgb8(0.5), srgb8(0.2), 128];
    assert_eq!(px.len(), 16 * 9 * 4);
    for p in px.as_chunks::<4>().0.iter() {
        assert!(p.iter().zip(expect).all(|(a, b)| close(*a, b)), "BGRA {p:?}, expected {expect:?}");
    }
    println!("child read {} pixels from `{name}`", px.len() / 4);
}
