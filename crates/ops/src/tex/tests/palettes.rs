//! Palette params on WARP: `tex.ramp` and `tex.lookup` from a named palette, a shader's palette
//! param from the project's own file (hot reloaded) and picked by a bound channel.

use std::time::{Duration, Instant};

use penumbra_core::palette::{self, to_linear};
use penumbra_core::{Name, ParamPath, Value};
use penumbra_graph::Op;

use super::{Rig, connect, create, gpu, scratch, set, size};

fn near(got: [f32; 4], want: [f64; 3]) -> bool {
    got.iter().zip(want).all(|(g, w)| (f64::from(*g) - w).abs() < 0.01)
}

fn linear(hex: u32) -> [f64; 3] {
    let byte = |s: u32| to_linear(f64::from((hex >> s) & 0xff) / 255.0);
    [byte(16), byte(8), byte(0)]
}

#[test]
fn ramps_and_lookups_take_palettes() {
    let Some(gpu) = gpu() else { return };
    let [w, h] = size();
    let ops = [
        create(
            "/steps",
            "tex.ramp",
            &[w.clone(), h.clone(), ("palette", "primaries".into()), ("steps", true.into())],
        ),
        create("/smooth", "tex.ramp", &[w.clone(), h.clone(), ("palette", "000000 ffffff".into())]),
        create("/grey", "tex.ramp", &[w, h]),
        create("/mapped", "tex.lookup", &[("palette", "magma".into())]),
        connect("/grey", "/mapped", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    let px = rig.pixels("/steps");
    assert!(near(px.get(10, 5).unwrap(), [1.0, 0.0, 0.0]), "{:?}", px.get(10, 5));
    assert!(near(px.get(48, 5).unwrap(), [0.0, 1.0, 0.0]), "{:?}", px.get(48, 5));
    assert!(near(px.get(86, 5).unwrap(), [0.0, 0.0, 1.0]), "{:?}", px.get(86, 5));
    let px = rig.pixels("/smooth");
    let [r, ..] = px.get(48, 5).unwrap();
    assert!((r - 0.5).abs() < 0.02, "{r}");
    let px = rig.pixels("/mapped");
    assert!(near(px.get(0, 5).unwrap(), linear(0x000004)), "{:?}", px.get(0, 5));
    // The last pixel's centre is 95.5 / 96 along.
    let magma = palette::resolve(&"magma".into(), &[]).unwrap().unwrap();
    let [r, g, b, _] = magma.at(95.5 / 96.0);
    assert!(near(px.get(95, 5).unwrap(), [r, g, b]), "{:?}", px.get(95, 5));
    // A bad value fails the ramp with the reason, not the app.
    rig.edit(&[set("/steps.palette", "nosuch".into())]);
    let id = rig.id("/steps");
    rig.cook("/steps");
    let s = rig.cooker.status(id).unwrap();
    assert!(s.error.as_deref().is_some_and(|e| e.contains("no palette `nosuch`")), "{s:?}");
}

const SHADER: &str = "\
// @param look palette = blood
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    return pn_palette(params.look, 0);
}
";

/// Cooks `path` until its first pixel is `want`, letting the watcher and the clock move.
fn until_colour(rig: &mut Rig, path: &str, want: [f64; 3]) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        rig.cook(path);
        if let Some(t) = rig.tex(path)
            && let Ok(px) = rig.gpu.read(&t)
            && near(px.get(0, 0).unwrap_or_default(), want)
        {
            return;
        }
        assert!(Instant::now() < deadline, "{path} never showed {want:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn shaders_take_the_projects_palettes_and_channels_pick_them() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("palettes");
    std::fs::write(dir.join("look.wgsl"), SHADER).unwrap();
    std::fs::write(dir.join(palette::FILE), "# ours\nmine: ff0000 00ff00\nbroken line\n").unwrap();
    let ops = [create(
        "/s",
        "tex.shader",
        &[("file", "look.wgsl".into()), ("width", Value::Int(4)), ("height", Value::Int(4))],
    )];
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    until_colour(&mut rig, "/s", linear(0x31aaa9));
    rig.edit(&[set("/s.look", "mine".into())]);
    until_colour(&mut rig, "/s", [1.0, 0.0, 0.0]);
    let warning = rig.cooker.status(rig.id("/s")).unwrap().warning.unwrap_or_default();
    assert!(warning.contains("palettes.txt: line 3"), "{warning}");
    // An edit of the file shows without touching the node.
    std::fs::write(dir.join(palette::FILE), "mine: 0000ff\n").unwrap();
    until_colour(&mut rig, "/s", [0.0, 0.0, 1.0]);
    // A channel picks by place: the built-in set first (0 is sea), then the project's.
    let mine = palette::builtin().len() as f64;
    rig.edit(&[
        create("/k", "chan.constant", &[("name0", "pick".into()), ("value0", Value::Float(0.0))]),
        Op::Bind {
            param: ParamPath::parse("/s.look").unwrap(),
            component: None,
            from: "/k".parse().unwrap(),
            channel: Some(Name::new("pick").unwrap()),
        },
    ]);
    until_colour(&mut rig, "/s", linear(0x8ad6d1));
    rig.edit(&[set("/k.value0", Value::Float(mine))]);
    until_colour(&mut rig, "/s", [0.0, 0.0, 1.0]);
    // An unknown name warns and falls back to the default.
    rig.edit(&[Op::Unbind { param: ParamPath::parse("/s.look").unwrap(), component: None }]);
    rig.edit(&[set("/s.look", "gone".into())]);
    until_colour(&mut rig, "/s", linear(0x31aaa9));
}
