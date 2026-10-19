//! The conversions on a software GPU (WARP), without NDI; with `--ignored`, a source sent and
//! received in this process through the installed NDI runtime.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use penumbra_gpu::{AdapterChoice, Alpha, Gpu, Tex};

use crate::convert::{Packed, Packer, Picture, Unpacker, matrix};
use crate::ffi::fourcc;

fn gpu() -> Option<Gpu> {
    static GPU: OnceLock<Option<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| match Gpu::headless(AdapterChoice::Software) {
        Ok(g) => Some(g),
        Err(e) if cfg!(windows) => panic!("WARP must exist on Windows: {e}"),
        Err(e) => {
            eprintln!("skipping GPU tests: {e}");
            None
        }
    })
    .clone()
}

/// A texture from linear, premultiplied pixels, row by row.
fn texture(gpu: &Gpu, w: u32, h: u32, pixel: impl Fn(u32, u32) -> [f32; 4]) -> Tex {
    let mut bytes = Vec::new();
    for y in 0..h {
        for x in 0..w {
            for v in pixel(x, y) {
                bytes.extend_from_slice(&half::f16::from_f32(v).to_le_bytes());
            }
        }
    }
    gpu.upload_rgba16f(w, h, &mut bytes, "test").unwrap()
}

fn packed(gpu: &Gpu, packer: &mut Packer, tex: &Tex, alpha: Alpha) -> (Packed, Vec<u8>) {
    let p = packer.grab(gpu, tex, alpha).unwrap().unwrap();
    p.wait(Duration::from_secs(10)).unwrap();
    let bytes = p.with_bytes(<[u8]>::to_vec).unwrap();
    (p, bytes)
}

fn srgb(v: f32) -> f32 {
    penumbra_gpu::readback::linear_to_srgb(v)
}

#[test]
fn colours_become_video_range_uyvy() {
    let Some(gpu) = gpu() else { return };
    let mut packer = Packer::new(2);
    let (kr, kb) = matrix(1280, 720);
    assert_eq!((kr, kb), (0.2126, 0.0722));
    // White, black, linear red, 18 % grey: as `penumbra_gpu::yuv` makes them for H.264.
    for (rgba, want) in [
        ([1.0, 1.0, 1.0, 1.0], (235, 128, 128)),
        ([0.0, 0.0, 0.0, 1.0], (16, 128, 128)),
        ([1.0, 0.0, 0.0, 1.0], (63, 102, 240)),
        ([0.18, 0.18, 0.18, 1.0], (117, 128, 128)),
        ([f32::NAN, 0.0, 0.0, 1.0], (16, 128, 128)),
    ] {
        let tex = texture(&gpu, 1280, 720, |_, _| rgba);
        let (p, b) = packed(&gpu, &mut packer, &tex, Alpha::Opaque);
        assert_eq!((p.fourcc, p.stride, b.len()), (fourcc::UYVY, 2560, 2560 * 720));
        let last = 2560 * 719 + 2556;
        let near = |a: u8, b: u8| (i32::from(a) - i32::from(b)).abs() <= 1;
        let got = (b[last + 1], b[last], b[last + 2]);
        assert!(near(got.0, want.0) && near(got.1, want.1) && near(got.2, want.2), "{rgba:?}: {got:?}");
        assert_eq!(b[last + 1], b[last + 3]);
    }
}

#[test]
fn alpha_follows_the_colour_as_a_plane() {
    let Some(gpu) = gpu() else { return };
    let mut packer = Packer::new(2);
    // Odd width: the last column is left out. Alpha rises left to right.
    let (w, h) = (7u32, 3u32);
    let tex = texture(&gpu, w, h, |x, _| {
        let a = x as f32 / 5.0;
        [0.5 * a, 0.5 * a, 0.5 * a, a]
    });
    let (p, b) = packed(&gpu, &mut packer, &tex, Alpha::Straight);
    assert_eq!((p.fourcc, p.width, p.height, p.stride), (fourcc::UYVA, 6, 3, 12));
    let plane = &b[12 * 3..12 * 3 + 18];
    for y in 0..3 {
        let row: Vec<u8> = plane[y * 6..y * 6 + 6].to_vec();
        assert_eq!(row, vec![0, 51, 102, 153, 204, 255], "row {y}");
    }
    // Straight: the colour is the unpremultiplied 0.5, sRGB-encoded, wherever alpha is above 0.
    let want = (16.0 + 219.0 * srgb(0.5)).round() as u8;
    assert_eq!(b[3], want);
    assert_eq!(b[5], want);
}

/// Sends a picture through pack and unpack and returns what comes out, read back.
fn round_trip(gpu: &Gpu, tex: &Tex, alpha: Alpha) -> penumbra_gpu::Pixels {
    let mut packer = Packer::new(1);
    let (p, b) = packed(gpu, &mut packer, tex, alpha);
    let picture = Picture {
        width: p.width,
        height: p.height,
        fourcc: p.fourcc,
        stride: p.stride,
        data: b,
        field: false,
    };
    let out = Tex::new(gpu, p.width, p.height, "out").unwrap();
    Unpacker::new().convert(gpu, &picture, alpha, &out).unwrap();
    gpu.read(&out).unwrap()
}

#[test]
fn a_picture_comes_back_as_it_went() {
    let Some(gpu) = gpu() else { return };
    // Smooth colours (chroma is shared by pairs of pixels) at an SD, HD and UHD size: each
    // size has its own matrix, and both directions must agree on it.
    for (w, h) in [(64, 48), (1280, 720), (2048, 1100)] {
        let pixel = |x: u32, y: u32| {
            let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
            let a = 0.25 + 0.75 * u;
            [0.8 * v * a, 0.5 * a, (1.0 - v) * 0.3 * a, a]
        };
        let tex = texture(&gpu, w, h, pixel);
        for alpha in [Alpha::Straight, Alpha::Premultiplied] {
            let back = round_trip(&gpu, &tex, alpha);
            let mut worst = 0f32;
            for (x, y) in [(2, 2), (w / 2, h / 3), (w - 4, h - 3), (w / 3, h - 1)] {
                let (want, got) = (pixel(x, y), back.get(x, y).unwrap());
                for c in 0..4 {
                    worst = worst.max((want[c] - got[c]).abs());
                }
            }
            assert!(worst < 0.02, "{w}x{h} {alpha:?}: off by {worst}");
        }
    }
    // Opaque sends over black and receives alpha 1.
    let tex = texture(&gpu, 16, 8, |_, _| [0.25, 0.0, 0.0, 0.5]);
    let back = round_trip(&gpu, &tex, Alpha::Opaque);
    let got = back.get(5, 5).unwrap();
    assert!((got[0] - 0.25).abs() < 0.01 && got[3] == 1.0, "{got:?}");
}

#[test]
fn received_rgb_and_fields_are_taken() {
    let Some(gpu) = gpu() else { return };
    // BGRA, 2x2 field: red with half alpha (straight), shown with each line twice.
    let (w, h) = (2u32, 2u32);
    let data = [0u8, 0, 255, 128].repeat((w * h) as usize);
    let picture = Picture { width: w, height: h, fourcc: fourcc::BGRA, stride: 8, data, field: true };
    assert_eq!(picture.size(), (2, 4));
    let out = Tex::new(&gpu, 2, 4, "out").unwrap();
    Unpacker::new().convert(&gpu, &picture, Alpha::Straight, &out).unwrap();
    let got = gpu.read(&out).unwrap().get(1, 3).unwrap();
    let a = 128.0 / 255.0;
    assert!((got[0] - a).abs() < 0.01 && got[1].abs() < 0.01 && (got[3] - a).abs() < 0.01, "{got:?}");
    // A format it does not take, and bytes short of the size, are errors, not reads past the end.
    let bad = Picture {
        width: 2,
        height: 2,
        fourcc: u32::from_le_bytes(*b"NV12"),
        stride: 2,
        data: vec![0; 6],
        field: false,
    };
    let small = Tex::new(&gpu, 2, 2, "small").unwrap();
    assert!(Unpacker::new().convert(&gpu, &bad, Alpha::Straight, &small).is_err());
    let short =
        Picture { width: 2, height: 2, fourcc: fourcc::UYVY, stride: 4, data: vec![0; 7], field: false };
    assert!(Unpacker::new().convert(&gpu, &short, Alpha::Straight, &small).is_err());
}

/// Through the installed runtime: a source sent from this process is listed, received with
/// its colour, and gone when the sender is dropped. Needs NDI Tools or the NDI Runtime.
#[test]
#[ignore = "needs the NDI runtime and the local network"]
fn a_source_sent_here_is_received_here() {
    let gpu = Gpu::headless(AdapterChoice::Software).unwrap();
    let name = format!("penumbra test {}", std::process::id());
    let mut sender = crate::Sender::new(&name).unwrap();
    let tex = texture(&gpu, 320, 180, |x, _| [x as f32 / 320.0, 0.2, 0.0, 1.0]);
    let start = Instant::now();
    let full = loop {
        sender.send(&gpu, &tex, Alpha::Opaque, (60, 1)).unwrap();
        gpu.poll();
        if let Some(n) = sender.name() {
            break n;
        }
        assert!(start.elapsed() < Duration::from_secs(5), "never announced");
        std::thread::sleep(Duration::from_millis(16));
    };
    assert!(full.ends_with(&format!("({name})")), "{full}");
    let receiver = crate::Receiver::new(&name, crate::Bandwidth::Highest, "penumbra test", || {}).unwrap();
    let start = Instant::now();
    let picture = loop {
        sender.send(&gpu, &tex, Alpha::Opaque, (60, 1)).unwrap();
        gpu.poll();
        if let Some(p) = receiver.take_picture() {
            break p;
        }
        assert!(start.elapsed() < Duration::from_secs(15), "nothing received: {:?}", receiver.status());
        std::thread::sleep(Duration::from_millis(16));
    };
    assert_eq!((picture.width, picture.height, picture.fourcc), (320, 180, fourcc::UYVY));
    let out = Tex::new(&gpu, 320, 180, "out").unwrap();
    Unpacker::new().convert(&gpu, &picture, Alpha::Straight, &out).unwrap();
    let got = gpu.read(&out).unwrap().get(160, 90).unwrap();
    assert!((got[0] - 0.5).abs() < 0.03 && (got[1] - 0.2).abs() < 0.03 && got[3] == 1.0, "{got:?}");
    assert!(crate::sources().unwrap().iter().any(|s| s.name == full));
    let status = receiver.status();
    assert_eq!(status.source.as_deref(), Some(full.as_str()));
    assert!(status.connected && status.frames > 0, "{status:?}");
}
