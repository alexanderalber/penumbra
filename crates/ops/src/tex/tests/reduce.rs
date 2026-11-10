//! `chan.from_tex` measures on WARP: exact counts at 1920 x 1080, every measure, areas, a
//! 32-bit float input, and the half-float fallback of a GPU without 32-bit float targets.

use penumbra_core::{Chans, Value};
use penumbra_gpu::{AdapterChoice, Gpu};
use penumbra_graph::Op;

use super::{Rig, chans_of, connect, create, gpu, scratch, set, uv_picture};

/// Red is 1 on a sparse pattern of pixels and 0 elsewhere, green steps 0, 0.25, 0.5, 0.75
/// across, blue is 0 and alpha 1.
const PATTERN: &str = "@fragment fn main(v: PnVertex) -> @location(0) vec4f { \
    let p = vec2u(floor(v.pos.xy)); \
    let on = select(0.0, 1.0, (p.x * 7u + p.y * 13u) % 11u == 0u); \
    return vec4f(on, f32(p.x % 4u) * 0.25, 0.0, 1.0); }";

const W: u32 = 1920;
const H: u32 = 1080;

/// The pixels where [`PATTERN`]'s red is 1.
fn ones() -> u64 {
    let mut n = 0;
    for y in 0..H {
        for x in 0..W {
            n += u64::from((x * 7 + y * 13) % 11 == 0);
        }
    }
    n
}

fn pattern(format: &str) -> Op {
    create(
        "/p",
        "tex.shader",
        &[
            ("file", "pattern.wgsl".into()),
            ("resolution", "custom".into()),
            ("width", Value::Int(i64::from(W))),
            ("height", Value::Int(i64::from(H))),
            ("format", format.into()),
        ],
    )
}

/// The channels of a `chan.from_tex` with `params` reading `source`, on `gpu`.
fn measure(gpu: Gpu, name: &str, source: Op, params: &[(&str, Value)]) -> Chans {
    let dir = scratch(name);
    std::fs::write(dir.join("pattern.wgsl"), PATTERN).unwrap();
    let ops = [source, create("/c", "chan.from_tex", params), connect("/p", "/c", 0)];
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    let c = chans_of(&mut rig, "/c");
    let s = rig.cooker.status(rig.id("/c")).unwrap();
    assert!(s.warning.is_none() && s.error.is_none(), "{s:?}");
    c
}

fn value(c: &Chans, name: &str) -> f64 {
    c.value(Some(name)).unwrap_or_else(|| panic!("no channel {name}: {c:?}"))
}

#[test]
fn the_sum_of_a_picture_of_0_and_1_is_an_exact_count() {
    let Some(gpu) = gpu() else { return };
    let pixels = f64::from(W * H);
    let n = ones() as f64;
    let c = measure(gpu.clone(), "reduce_sum", pattern("16f"), &[("measure", "sum".into())]);
    assert_eq!(value(&c, "r"), n);
    assert_eq!(value(&c, "a"), pixels);
    // 480 columns of each step, 1080 rows: 1080 * 480 * (0 + 0.25 + 0.5 + 0.75).
    assert_eq!(value(&c, "g"), 777_600.0);
    assert_eq!(value(&c, "b"), 0.0);
    let c = measure(gpu.clone(), "reduce_mean", pattern("16f"), &[]);
    assert!((value(&c, "r") - n / pixels).abs() < 1e-12, "{c:?}");
    assert!((value(&c, "g") - 0.375).abs() < 1e-12, "{c:?}");
    let c = measure(
        gpu.clone(),
        "reduce_coverage",
        pattern("16f"),
        &[("measure", "coverage".into()), ("threshold", Value::Float(0.3))],
    );
    assert!((value(&c, "r") - n / pixels).abs() < 1e-12, "{c:?}");
    assert!((value(&c, "g") - 0.5).abs() < 1e-12, "green above 0.3: 0.5 and 0.75, {c:?}");
    assert_eq!(value(&c, "a"), 1.0);
    let c = measure(gpu.clone(), "reduce_min", pattern("16f"), &[("measure", "min".into())]);
    assert_eq!((value(&c, "r"), value(&c, "g"), value(&c, "a")), (0.0, 0.0, 1.0));
    let c = measure(gpu, "reduce_max", pattern("16f"), &[("measure", "max".into())]);
    assert_eq!((value(&c, "r"), value(&c, "g"), value(&c, "b")), (1.0, 0.75, 0.0));
}

#[test]
fn a_32_bit_float_input_is_measured_in_32_bits() {
    let Some(gpu) = gpu() else { return };
    // 1 + 2^-12 per pixel: half floats would round it to 1.
    let fine = "@fragment fn main(v: PnVertex) -> @location(0) vec4f { \
        return vec4f(1.0 + 1.0 / 4096.0, 0.0, 0.0, 1.0); }";
    let dir = scratch("reduce_32f");
    std::fs::write(dir.join("fine.wgsl"), fine).unwrap();
    let ops = [
        create(
            "/p",
            "tex.shader",
            &[
                ("file", "fine.wgsl".into()),
                ("resolution", "custom".into()),
                ("width", Value::Int(64)),
                ("height", Value::Int(32)),
                ("format", "32f".into()),
            ],
        ),
        create("/c", "chan.from_tex", &[("components", "red".into())]),
        connect("/p", "/c", 0),
    ];
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    let c = chans_of(&mut rig, "/c");
    let want = 1.0 + 1.0 / 4096.0;
    assert!((value(&c, "r") - want).abs() < 1e-6, "{c:?}");
}

#[test]
fn each_area_is_measured_on_its_own() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("reduce_areas");
    let ops = [
        uv_picture(&dir),
        create(
            "/s",
            "chan.from_tex",
            &[("columns", Value::Int(4)), ("rows", Value::Int(2)), ("measure", "sum".into())],
        ),
        connect("/uv", "/s", 0),
        create(
            "/lo",
            "chan.from_tex",
            &[("columns", Value::Int(4)), ("measure", "min".into()), ("components", "red".into())],
        ),
        connect("/uv", "/lo", 0),
        create(
            "/hi",
            "chan.from_tex",
            &[("rows", Value::Int(2)), ("measure", "max".into()), ("components", "luminance".into())],
        ),
        connect("/uv", "/hi", 0),
    ];
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    // 1000 x 600 in 4 x 2 areas of 250 x 300 pixels.
    let s = chans_of(&mut rig, "/s");
    for name in ["a0", "a1"] {
        assert!(s.get(name).unwrap().samples.iter().all(|a| *a == 75_000.0), "{s:?}");
    }
    // The u ramp is (x + 0.5) / 1000 at pixel x, in half floats; column i starts at pixel 250 i.
    let lo = chans_of(&mut rig, "/lo");
    for (i, r) in lo.get("r").unwrap().samples.iter().enumerate() {
        let want = (250.0 * i as f64 + 0.5) / 1000.0;
        assert!((r - want).abs() < 5e-4, "column {i}: {r}");
    }
    // Brightest pixel of each half: the bottom right one of the half.
    let hi = chans_of(&mut rig, "/hi");
    let lum = |u: f64, v: f64| 0.2126 * u + 0.7152 * v;
    assert!((hi.value(Some("lum0")).unwrap() - lum(0.9995, 299.5 / 600.0)).abs() < 1e-3, "{hi:?}");
    assert!((hi.value(Some("lum1")).unwrap() - lum(0.9995, 599.5 / 600.0)).abs() < 1e-3, "{hi:?}");
}

#[test]
fn without_32_bit_float_targets_the_count_stays_exact() {
    let Ok(gpu) = Gpu::headless_without_float_filtering(AdapterChoice::Software) else { return };
    assert!(!gpu.float_targets());
    let c = measure(gpu.clone(), "reduce_half", pattern("16f"), &[("measure", "sum".into())]);
    assert_eq!(value(&c, "r"), ones() as f64);
    assert_eq!(value(&c, "a"), f64::from(W * H));
    let c = measure(gpu.clone(), "reduce_half_mean", pattern("16f"), &[]);
    assert!((value(&c, "g") - 0.375).abs() < 1e-9, "{c:?}");
    assert_eq!(gpu.errors().count, 0);
}

/// A 3 x 2 picture whose texel at x, y holds red x + 10 y and green 100 + x + 10 y.
const STRIP: &str = "@fragment fn main(v: PnVertex) -> @location(0) vec4f { \
    let p = floor(v.pos.xy); \
    let n = p.x + 10.0 * p.y; \
    return vec4f(n, 100.0 + n, 0.5, 1.0); }";

fn strip(width: i64, height: i64) -> Op {
    create(
        "/p",
        "tex.shader",
        &[
            ("file", "strip.wgsl".into()),
            ("resolution", "custom".into()),
            ("width", Value::Int(width)),
            ("height", Value::Int(height)),
            ("format", "32f".into()),
        ],
    )
}

fn named(c: &Chans) -> Vec<(String, f64)> {
    c.channels().iter().map(|ch| (ch.name.to_string(), ch.samples[0])).collect()
}

#[test]
fn every_texel_becomes_named_channels_row_by_row() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("reduce_texels");
    std::fs::write(dir.join("strip.wgsl"), STRIP).unwrap();
    let texels = |names: &str| {
        create(
            "/c",
            "chan.from_tex",
            &[("layout", "texels".into()), ("components", "rg".into()), ("names", names.into())],
        )
    };
    let ops = [strip(3, 2), texels("a b - d"), connect("/p", "/c", 0)];
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    let c = chans_of(&mut rig, "/c");
    assert!(c.channels().iter().all(|ch| ch.samples.len() == 1), "{c:?}");
    let want: Vec<(String, f64)> = [
        ("a", 0.0),
        ("b", 100.0),
        ("d", 101.0),
        ("chan4", 2.0),
        ("chan5", 102.0),
        ("chan6", 10.0),
        ("chan7", 110.0),
        ("chan8", 11.0),
        ("chan9", 111.0),
        ("chan10", 12.0),
        ("chan11", 112.0),
    ]
    .iter()
    .map(|(n, v)| ((*n).to_string(), *v))
    .collect();
    assert_eq!(named(&c), want);
    let s = rig.cooker.status(rig.id("/c")).unwrap();
    assert!(s.warning.is_none() && s.error.is_none(), "{s:?}");

    // A size change brings as many channels as the picture now has texels.
    rig.edit(&[set("/p.width", Value::Int(1)), set("/c.names", "x y z".into())]);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let c = loop {
        let c = chans_of(&mut rig, "/c");
        if c.channels().len() == 4 || std::time::Instant::now() > deadline {
            break c;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    let want: Vec<(String, f64)> = [("x", 0.0), ("y", 100.0), ("z", 10.0), ("chan3", 110.0)]
        .iter()
        .map(|(n, v)| ((*n).to_string(), *v))
        .collect();
    assert_eq!(named(&c), want);

    // More names than channels: the rest are named in a warning.
    rig.edit(&[set("/c.names", "a b c d e f".into())]);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let c = chans_of(&mut rig, "/c");
        let s = rig.cooker.status(rig.id("/c")).unwrap();
        if c.get("a").is_some() {
            let w = s.warning.unwrap_or_default();
            assert!(w.contains("e f not used"), "{w}");
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{c:?}");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn texels_refuse_a_large_picture_and_areas_take_names_too() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("reduce_texels_cap");
    std::fs::write(dir.join("strip.wgsl"), STRIP).unwrap();
    let ops = [
        strip(40, 30),
        create("/big", "chan.from_tex", &[("layout", "texels".into())]),
        connect("/p", "/big", 0),
        create("/area", "chan.from_tex", &[("components", "rg".into()), ("names", "mean_r".into())]),
        connect("/p", "/area", 0),
        create("/rows", "chan.from_tex", &[("rows", Value::Int(2)), ("components", "rg".into())]),
        connect("/p", "/rows", 0),
    ];
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    // Areas: the names rename in order, the rest keep their own names; rg gives r and g.
    let area = chans_of(&mut rig, "/area");
    let names: Vec<&str> = area.names().map(|n| n.as_str()).collect();
    assert_eq!(names, ["mean_r", "g"]);
    // Mean of x + 10 y over 40 x 30: 19.5 + 10 * 14.5.
    assert!((value(&area, "mean_r") - 164.5).abs() < 1e-3, "{area:?}");
    let rows = chans_of(&mut rig, "/rows");
    let names: Vec<&str> = rows.names().map(|n| n.as_str()).collect();
    assert_eq!(names, ["r0", "g0", "r1", "g1"]);
    // 1200 texels are more than layout texels reads.
    let id = rig.id("/big");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let error = loop {
        rig.gpu.poll();
        rig.cook("/big");
        if let Some(e) = rig.cooker.status(id).unwrap().error {
            break e;
        }
        assert!(std::time::Instant::now() < deadline, "no error for 1200 texels");
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    assert!(error.contains("40 x 30 = 1200 texels") && error.contains("at most 1024"), "{error}");
}
