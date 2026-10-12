//! `tex.layer` on WARP: a PSD built from code (a group, a mask, opacity, a hidden layer) and a
//! layer directory of PNGs.

use penumbra_core::{Table, Value};
use penumbra_graph::Port;
use photocraft_psd::builder::{GroupSpec, LayerSpec, MaskSpec, PixelData, PsdBuilder};
use photocraft_psd::layer::Rect;

use super::{Rig, create, gpu, scratch, set};

fn solid(w: u32, h: u32, rgba: [u8; 4]) -> PixelData {
    PixelData::Rgba8((0..w * h).flat_map(|_| rgba).collect())
}

/// 64 x 40: a dark blue background, a group `sign` with a half-transparent red `panel` (its
/// mask shows the left half) and a hidden green layer, a white `text` on top.
pub(super) fn psd() -> Vec<u8> {
    let mut b = PsdBuilder::new(64, 40);
    b.push_layer(LayerSpec::new("background", 0, 0, 64, 40, solid(64, 40, [10, 20, 80, 255])));
    b.begin_group(GroupSpec::new("sign"));
    let mut panel = LayerSpec::new("panel", 8, 6, 24, 16, solid(24, 16, [255, 0, 0, 255]));
    panel.opacity = 128;
    panel.mask = Some(MaskSpec {
        rect: Rect::from_xywh(8, 6, 24, 16),
        data: (0..16).flat_map(|_| (0..24).map(|x| if x < 12 { 255 } else { 0 })).collect(),
        default_color: 0,
        disabled: false,
    });
    b.push_layer(panel);
    let mut hidden = LayerSpec::new("hidden", 40, 10, 10, 10, solid(10, 10, [0, 255, 0, 255]));
    hidden.visible = false;
    b.push_layer(hidden);
    b.end_group().unwrap();
    b.push_layer(LayerSpec::new("text", 30, 20, 20, 10, solid(20, 10, [255, 255, 255, 255])));
    b.composite(solid(64, 40, [128, 128, 128, 255]));
    b.to_bytes().unwrap()
}

fn table(rig: &Rig, path: &str) -> Table {
    rig.cooker.output(Port { node: rig.id(path), output: 1 }).unwrap().get::<Table>().unwrap().clone()
}

fn px(p: &penumbra_gpu::Pixels, x: u32, y: u32) -> [f32; 4] {
    p.data[(y * p.width + x) as usize]
}

/// Cooks until `path` fails with an error containing `needle` (the previous error stays until
/// the new job is done).
fn error(rig: &mut Rig, path: &str, needle: &str) {
    let id = rig.id(path);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        rig.cooker.cook(&rig.net, rig.clock.now(), &[id], &[]);
        let e = rig.cooker.status(id).unwrap().error;
        if e.as_deref().is_some_and(|e| e.contains(needle)) {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "{path}: no error with `{needle}`: {e:?}");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn psd_layers_with_groups_masks_and_opacity() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("psd");
    std::fs::write(dir.join("poster.psd"), psd()).unwrap();
    let ops = vec![
        create("/panel", "tex.layer", &[("file", "poster.psd".into()), ("layer", "panel".into())]),
        create("/merged", "tex.layer", &[("file", "poster.psd".into())]),
    ];
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    let p = rig.pixels("/panel");
    assert_eq!((p.width, p.height), (64, 40));
    // Left half of the panel: red at half opacity, premultiplied; right half masked; outside empty.
    let red = px(&p, 10, 8);
    assert!((red[0] - 0.5).abs() < 0.01 && red[1] == 0.0 && (red[3] - 0.5).abs() < 0.01, "{red:?}");
    assert_eq!(px(&p, 28, 8)[3], 0.0);
    assert_eq!(px(&p, 2, 2), [0.0; 4]);
    let t = table(&rig, "/panel");
    let paths: Vec<&str> = t.rows().iter().skip(1).map(|r| r[2].as_str()).collect();
    assert_eq!(paths, ["background", "sign/panel", "sign/hidden", "sign", "text"]);
    assert_eq!(t.rows()[3][4], "0", "hidden is not visible");
    assert_eq!(t.rows()[4][3], "1", "sign is a group");
    assert_eq!(t.rows()[2][7..], ["8", "6", "24", "16"]);
    // The merged picture is the file's own.
    let m = rig.pixels("/merged");
    assert!((px(&m, 5, 5)[0] - penumbra_gpu::readback::srgb_to_linear(128.0 / 255.0)).abs() < 0.01);
    // By path and by index, unplaced: the layer's own rectangle.
    for spec in ["sign/panel", "#1"] {
        let v = rig.version("/panel");
        rig.edit(&[set("/panel.layer", spec.into()), set("/panel.placed", Value::Bool(false))]);
        let tex = rig.cook_until_new("/panel", v);
        assert_eq!(tex.size(), (24, 16), "{spec}");
        rig.edit(&[set("/panel.placed", Value::Bool(true))]);
    }
    // A group or an unknown name is an error saying what there is.
    rig.edit(&[set("/panel.layer", "sign".into())]);
    error(&mut rig, "/panel", "is a group");
    rig.edit(&[set("/panel.layer", "nope".into())]);
    error(&mut rig, "/panel", "sign/panel");
    // A broken file is an error naming it.
    std::fs::write(dir.join("broken.psd"), &psd()[..300]).unwrap();
    rig.edit(&[set("/panel.file", "broken.psd".into()), set("/panel.layer", "panel".into())]);
    error(&mut rig, "/panel", "broken.psd");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_layer_directory_stacks_its_images_by_name() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("layerdir");
    let layers = dir.join("layers");
    std::fs::create_dir_all(&layers).unwrap();
    let png = |rgba: [u8; 4]| {
        let data: Vec<u8> = (0..64).flat_map(|_| rgba).collect();
        let img =
            photocraft_codecs::Image::from_u8(8, 8, photocraft_codecs::ChannelLayout::Rgba, data).unwrap();
        photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &Default::default()).unwrap()
    };
    std::fs::write(layers.join("02_top.png"), png([255, 255, 255, 128])).unwrap();
    std::fs::write(layers.join("01_bottom.png"), png([255, 0, 0, 255])).unwrap();
    std::fs::write(layers.join("notes.txt"), "not a layer").unwrap();
    let ops = vec![
        create("/all", "tex.layer", &[("file", "layers".into())]),
        create("/top", "tex.layer", &[("file", "layers".into()), ("layer", "02_top".into())]),
    ];
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    let all = rig.pixels("/all");
    let a = px(&all, 3, 3);
    // White at alpha 0.5 over opaque red.
    assert!((a[0] - 1.0).abs() < 0.01 && (a[1] - 0.502).abs() < 0.01 && (a[3] - 1.0).abs() < 1e-3, "{a:?}");
    let t = table(&rig, "/all");
    assert_eq!(t.rows().iter().skip(1).map(|r| r[1].as_str()).collect::<Vec<_>>(), ["01_bottom", "02_top"]);
    let top = rig.pixels("/top");
    assert!((px(&top, 0, 0)[3] - 0.502).abs() < 0.01);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A 640 x 360 poster for looking at by eye, written to `PENUMBRA_LOOK` when set: a gradient,
/// a soft sun, a group with a masked bar and a hidden layer.
#[test]
fn write_a_poster_to_look_at() {
    let Some(dir) = std::env::var_os("PENUMBRA_LOOK") else { return };
    let (w, h) = (640u32, 360u32);
    let mut b = PsdBuilder::new(w, h);
    let bg: Vec<u8> = (0..h)
        .flat_map(|y| (0..w).flat_map(move |x| [(40 + x * 120 / w) as u8, 20, (90 + y * 120 / h) as u8, 255]))
        .collect();
    b.push_layer(LayerSpec::new("sky", 0, 0, w, h, PixelData::Rgba8(bg)));
    let sun: Vec<u8> = (0..160u32)
        .flat_map(|y| {
            (0..160u32).flat_map(move |x| {
                let d = ((x as f32 - 80.0).hypot(y as f32 - 80.0) / 80.0).min(1.0);
                [255, 200, 60, ((1.0 - d * d) * 255.0) as u8]
            })
        })
        .collect();
    b.push_layer(LayerSpec::new("sun", 400, 40, 160, 160, PixelData::Rgba8(sun)));
    b.begin_group(GroupSpec::new("title"));
    let mut bar = LayerSpec::new("bar", 40, 240, 560, 70, solid(560, 70, [240, 240, 240, 255]));
    bar.opacity = 200;
    bar.mask = Some(MaskSpec {
        rect: Rect::from_xywh(40, 240, 560, 70),
        data: (0..70).flat_map(|_| (0..560).map(|x| (x * 255 / 559) as u8)).collect(),
        default_color: 0,
        disabled: false,
    });
    b.push_layer(bar);
    let mut hidden = LayerSpec::new("draft", 0, 0, 100, 100, solid(100, 100, [0, 255, 0, 255]));
    hidden.visible = false;
    b.push_layer(hidden);
    b.end_group().unwrap();
    b.composite(solid(w, h, [90, 40, 120, 255]));
    let dir = std::path::Path::new(&dir);
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("poster.psd"), b.to_bytes().unwrap()).unwrap();
}
