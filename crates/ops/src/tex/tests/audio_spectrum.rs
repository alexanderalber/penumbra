//! `chan.audio_spectrum`'s texture on WARP: its size, its rows against the `out` channel, the
//! scroll, a repeated block replacing the newest row, made only while read.

use penumbra_core::{Chans, Value};
use penumbra_gpu::{Pixels, Tex};
use penumbra_graph::{Op, Port};

use super::{Rig, connect, create, gpu, set};
use crate::audio::spectrum::SPECTRUM;

/// An oscillator into a spectrum with `params`, and a `tex.null` reading its texture.
fn rig(gpu: penumbra_gpu::Gpu, params: &[(&str, Value)]) -> Rig {
    let ops = vec![
        create(
            "/o",
            "audio.oscillator",
            &[("frequency", Value::Float(1000.0)), ("amplitude", Value::Float(0.3))],
        ),
        create("/s", SPECTRUM, params),
        connect("/o", "/s", 0),
        create("/view", "tex.null", &[]),
        texture_into("/view"),
    ];
    Rig::new(gpu, None, &ops)
}

fn texture_into(to: &str) -> Op {
    Op::Connect { from: "/s".parse().unwrap(), output: 1, to: to.parse().unwrap(), input: 0 }
}

/// Cooks the viewer one frame on and hands the spectrum's bands and its texture's pixels.
fn frame(rig: &mut Rig, advance: bool) -> (Vec<f64>, Pixels) {
    if advance {
        rig.clock.advance(1);
    }
    rig.cook("/view");
    let s = rig.id("/s");
    let status = rig.cooker.status(s).unwrap();
    assert_eq!((status.error, status.warning), (None, None));
    let bands =
        rig.cooker.output(Port { node: s, output: 0 }).and_then(|d| d.get::<Chans>()).unwrap().channels()[0]
            .samples
            .clone();
    let tex: Tex = rig.tex("/view").expect("the texture through tex.null");
    (bands, rig.gpu.read(&tex).unwrap())
}

fn row(px: &Pixels, y: u32) -> Vec<f32> {
    (0..px.width).map(|x| px.get(x, y).unwrap()[0]).collect()
}

fn near(row: &[f32], bands: &[f64]) -> bool {
    row.len() == bands.len() && row.iter().zip(bands).all(|(r, b)| (f64::from(*r) - b).abs() < 2e-3)
}

#[test]
fn spectrum_texture_rows_scroll_down_newest_on_top() {
    let Some(gpu) = gpu() else { return };
    let mut rig = rig(gpu, &[("bands", Value::Int(16)), ("history", Value::Int(4))]);
    let (b0, px) = frame(&mut rig, false);
    assert_eq!((px.width, px.height), (16, 4));
    assert!(near(&row(&px, 0), &b0), "{:?} vs {b0:?}", row(&px, 0));
    // Grey, opaque; the rows not filled yet are 0.
    let p = px.get(9, 0).unwrap();
    assert!(p[0] == p[1] && p[1] == p[2] && p[3] == 1.0, "{p:?}");
    assert_eq!(row(&px, 3), vec![0.0; 16]);
    let mut seen = vec![b0];
    for _ in 0..5 {
        let (b, px) = frame(&mut rig, true);
        seen.insert(0, b);
        for (y, b) in seen.iter().take(4).enumerate() {
            assert!(near(&row(&px, y as u32), b), "row {y}");
        }
    }
    // The tone shows in its band, at level about 1 - 10.5 / 60.
    let k = (16.0 * (1000.0f64 / 30.0).ln() / (16_000.0f64 / 30.0).ln()).floor() as u32;
    let (_, px) = frame(&mut rig, true);
    let loudest = (0..16).max_by(|a, b| px.get(*a, 0).unwrap()[0].total_cmp(&px.get(*b, 0).unwrap()[0]));
    assert_eq!(loudest, Some(k));

    // A param changed at the same tick: the newest row is replaced, nothing scrolls.
    let before = row(&px, 1);
    rig.edit(&[set("/s.unit", "linear".into())]);
    let (b, px) = frame(&mut rig, false);
    assert!(near(&row(&px, 0), &b));
    assert_eq!(row(&px, 1), before);
    // A new height starts the rows over.
    rig.edit(&[set("/s.history", Value::Int(2))]);
    let (b, px) = frame(&mut rig, true);
    assert_eq!((px.width, px.height), (16, 2));
    assert!(near(&row(&px, 0), &b));
    assert_eq!(row(&px, 1), vec![0.0; 16]);
}

#[test]
fn spectrum_texture_is_made_only_while_read() {
    let Some(gpu) = gpu() else { return };
    let mut rig = rig(gpu, &[]);
    rig.edit(&[
        Op::Disconnect { to: "/view".parse().unwrap(), input: 0 },
        create("/sink", "chan.null", &[]),
        connect("/s", "/sink", 0),
    ]);
    let s = rig.id("/s");
    let texture = |rig: &Rig| rig.cooker.output(Port { node: s, output: 1 }).cloned().unwrap();
    for _ in 0..3 {
        rig.clock.advance(1);
        rig.cook("/sink");
    }
    assert!(texture(&rig).is_skipped());
    assert!(!rig.cooker.status(s).unwrap().time_dependent);
    rig.edit(&[texture_into("/view")]);
    rig.cook("/view");
    let tex = rig.tex("/view").expect("made once read");
    assert_eq!(tex.size(), (32, 1));
    rig.clock.advance(1);
    rig.cook("/sink");
    assert!(texture(&rig).is_skipped(), "skipped again once nothing reads it");
}

/// Spectrograms for the eye in `E:/misc/temp/s99p3`: a log sweep from 40 Hz to 12 kHz over
/// 256 frames, and `audio.ambience`'s rain, both 128 bands by 256 rows. Run with
/// `cargo test -p penumbra-ops write_spectrum_pictures -- --ignored`.
#[test]
#[ignore]
fn write_spectrum_pictures() {
    let Some(gpu) = gpu() else { return };
    let dir = std::path::Path::new("E:/misc/temp/s99p3");
    std::fs::create_dir_all(dir).unwrap();
    let params = [("bands", Value::Int(128)), ("history", Value::Int(256)), ("release", Value::Float(0.1))];
    let mut rig = rig(gpu.clone(), &params);
    let mut last = None;
    for k in 0..256 {
        let hz = 40.0 * (12_000.0f64 / 40.0).powf(f64::from(k) / 255.0);
        rig.edit(&[set("/o.frequency", Value::Float(hz))]);
        last = Some(frame(&mut rig, true).1);
    }
    std::fs::write(dir.join("spectrum_sweep.png"), last.unwrap().to_png().unwrap()).unwrap();
    let ops = vec![
        create("/o", "audio.ambience", &[("preset", "rain".into()), ("density", Value::Float(0.6))]),
        create("/s", SPECTRUM, &params),
        connect("/o", "/s", 0),
        create("/view", "tex.null", &[]),
        texture_into("/view"),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    for _ in 0..255 {
        frame(&mut rig, true);
    }
    let px = frame(&mut rig, true).1;
    std::fs::write(dir.join("spectrum_rain.png"), px.to_png().unwrap()).unwrap();
}
