//! `tex.liquid` and `tex.liquid_emitter` on WARP: a block falls into a flat pool of the right
//! height, emitters pour where and as much as they say, open edges drain, a full budget warns,
//! reset, repeatability, a hitch, a squeeze and hostile input.

use penumbra_core::Value;
use penumbra_gpu::Pixels;
use penumbra_graph::{MAX_RUN, Op};

use super::{Rig, create, gpu, scratch, set};

/// The `fill` input, after the run of emitters.
const FILL: u32 = MAX_RUN as u32;

/// A liquid in a 4:3 picture of 128 x 96 pixels.
fn liquid(params: &[(&str, Value)]) -> Op {
    let mut all: Vec<(&str, Value)> = vec![
        ("width", Value::Int(128)),
        ("height", Value::Int(96)),
        ("grid", Value::Int(64)),
        ("particles", Value::Int(4096)),
    ];
    all.extend(params.iter().cloned());
    create("/liquid", "tex.liquid", &all)
}

fn wire(from: &str, output: u32, to: &str, input: u32) -> Op {
    Op::Connect { from: from.parse().unwrap(), output, to: to.parse().unwrap(), input }
}

/// A picture of 128 x 96 from a fragment shader body that returns `inside` as a bool for `v.uv`.
fn mask(path: &str, file: &str, dir: &std::path::Path, inside: &str) -> Op {
    let source = format!(
        "@fragment\nfn main(v: PnVertex) -> @location(0) vec4f {{\n    let uv = v.uv;\n    return vec4f(vec3f(select(0.0, 1.0, {inside})), 1.0);\n}}\n"
    );
    std::fs::write(dir.join(file), source).unwrap();
    create(
        path,
        "tex.shader",
        &[("file", file.into()), ("width", Value::Int(128)), ("height", Value::Int(96))],
    )
}

/// The liquid's outputs, read through nulls: `/v` the velocity, `/d` the density.
fn outputs() -> [Op; 4] {
    [
        create("/v", "tex.null", &[]),
        create("/d", "tex.null", &[]),
        wire("/liquid", 1, "/v", 0),
        wire("/liquid", 2, "/d", 0),
    ]
}

/// Moves the show on `frames` frames, one cook each, and reads `/liquid`, `/v` and `/d`.
fn run(rig: &mut Rig, frames: usize) -> (Pixels, Pixels, Pixels) {
    for f in 0..frames {
        // Keeps the software adapter's queue short.
        if f % 30 == 29 {
            rig.gpu.wait(std::time::Duration::from_secs(120)).unwrap();
        }
        rig.clock.advance(1);
        // As the engine does every frame: the counters come back when the device is polled.
        rig.gpu.poll();
        let ids = [rig.id("/liquid"), rig.id("/v"), rig.id("/d")];
        rig.cooker.cook(&rig.net, rig.clock.now(), &ids, &[]);
    }
    read(rig)
}

fn read(rig: &mut Rig) -> (Pixels, Pixels, Pixels) {
    let px = |rig: &Rig, path: &str| rig.gpu.read(&rig.tex(path).unwrap()).unwrap();
    (px(rig, "/liquid"), px(rig, "/v"), px(rig, "/d"))
}

fn finite(px: &Pixels) -> bool {
    px.stats().non_finite == 0
}

/// Particles in the liquid, from the density picture: four fill a cell at rest.
fn particles(d: &Pixels) -> f64 {
    d.data.iter().map(|c| f64::from(c[0])).sum::<f64>() * 4.0
}

/// Centre of the density, in picture heights from the top left.
fn centre(d: &Pixels) -> [f64; 2] {
    let (mut sum, mut x, mut y) = (0.0, 0.0, 0.0);
    let dx = 1.0 / f64::from(d.height);
    for j in 0..d.height {
        for i in 0..d.width {
            let m = f64::from(d.get(i, j).unwrap()[0]);
            sum += m;
            x += m * (f64::from(i) + 0.5) * dx;
            y += m * (f64::from(j) + 0.5) * dx;
        }
    }
    if sum > 0.0 { [x / sum, y / sum] } else { [f64::NAN; 2] }
}

/// The depth of the liquid in column `i`, picture heights: where the density, read upwards from
/// the floor, falls below half.
fn depth(d: &Pixels, i: u32) -> f64 {
    let dx = 1.0 / f64::from(d.height);
    let at = |j: u32| f64::from(d.get(i, j).unwrap()[0]);
    for j in (1..d.height).rev() {
        let (below, above) = (at(j), at(j - 1));
        if below >= 0.5 && above < 0.5 {
            // Between the centres of cells j and j - 1.
            let t = (below - 0.5) / (below - above);
            let y = (f64::from(j) + 0.5 - t) * dx;
            return 1.0 - y;
        }
    }
    0.0
}

/// Mean speed where there is liquid, picture heights per second, from the velocity picture
/// (red in widths).
fn mean_speed(v: &Pixels, d: &Pixels) -> f64 {
    let aspect = f64::from(v.width) / f64::from(v.height);
    let (mut sum, mut n) = (0.0, 0.0);
    for (c, m) in v.data.iter().zip(&d.data) {
        if m[0] > 0.5 {
            sum += (f64::from(c[0]) * aspect).hypot(f64::from(c[1]));
            n += 1.0;
        }
    }
    if n > 0.0 { sum / n } else { 0.0 }
}

fn warning(rig: &Rig) -> Option<String> {
    rig.cooker.status(rig.id("/liquid")).unwrap().warning
}

/// The dam of the compute fixture (`liquid_tests.rs` in the gpu crate), in picture units: a
/// square of a third of the picture's height at the left of the floor, in a picture 4:3, on 128
/// cells across, 4 particles per cell (4096). The fixture's numbers come from a room 3 m high and
/// are converted: gravity 9.81 m/s^2 to 3.27, sound speed 12 m/s to 4, viscosity 0.002 m^2/s to
/// 0.000222. The liquid comes to rest as in metres: a flat pool as high as its area over the
/// width. A run in metres at 65536 particles and 512 cells across gave 0.2508 m against 0.2499,
/// so 0.0836 against 0.0833 picture heights, and 0.016 m/s at 20 s, 0.0053 heights per second.
#[test]
fn a_filled_block_falls_and_rests_as_a_flat_pool_of_the_height_in_metres() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("liquid-dam");
    let mut ops = vec![
        liquid(&[("grid", Value::Int(128)), ("particles", Value::Int(8192))]),
        mask("/dam", "dam.wgsl", &dir, "uv.x < 0.25 && uv.y > 2.0 / 3.0"),
        wire("/dam", 0, "/liquid", FILL),
    ];
    ops.extend(outputs());
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    rig.pixels("/dam");
    let (_, _, d0) = run(&mut rig, 1);
    let start = particles(&d0);
    let (out, v, d) = run(&mut rig, 900);
    assert!(finite(&out) && finite(&v) && finite(&d));
    assert_eq!((d.width, d.height), (128, 96));
    // The fill: a third of a height squared, at four particles a cell of 1/96.
    let area = 1.0 / 9.0;
    assert!((start / (area * 4.0 * 96.0 * 96.0) - 1.0).abs() < 0.03, "{start} particles at the start");
    let expected = area / (4.0 / 3.0);
    let depths: Vec<f64> = (16..112).map(|i| depth(&d, i)).collect();
    let mean = depths.iter().sum::<f64>() / depths.len() as f64;
    let spread = depths.iter().fold(0.0f64, |a, h| a.max((h - mean).abs()));
    let speed = mean_speed(&v, &d);
    eprintln!(
        "dam: pool {mean:.4} heights (expected {expected:.4}, 0.0836 in metres), spread {spread:.4}, mean speed \
         {speed:.4} heights/s, particles {start:.0} then {:.0}",
        particles(&d)
    );
    assert!((mean / expected - 1.0).abs() < 0.04, "pool {mean} against {expected}");
    assert!(spread < 0.5 / 96.0, "not flat: {depths:?}");
    // In metres: 0.016 m/s at 20 s, 0.0053 picture heights per second.
    assert!(speed < 0.01, "still moving at {speed}");
    assert_eq!(warning(&rig), None);
}

/// An emitter to the left of the middle pours to the right without gravity: the liquid comes
/// in at its place, moves its way, at width times speed in area per second, and stops when
/// `on` is off.
#[test]
fn an_emitter_pours_at_its_place_its_way_and_its_rate_and_stops_when_off() {
    let Some(gpu) = gpu() else { return };
    let mut ops = vec![
        liquid(&[("gravity", Value::Vec2([0.0, 0.0])), ("drag", Value::Float(0.0))]),
        create(
            "/tap",
            "tex.liquid_emitter",
            &[
                ("position", Value::Vec2([0.25, 0.5])),
                ("width", Value::Float(0.1)),
                ("direction", Value::Vec2([1.0, 0.0])),
                ("speed", Value::Float(0.5)),
            ],
        ),
        wire("/tap", 0, "/liquid", 0),
    ];
    ops.extend(outputs());
    let mut rig = Rig::new(gpu, None, &ops);
    let (_, _, d) = run(&mut rig, 30);
    // Half a second at 0.1 x 0.5 heights^2 per second, four particles per cell of 1/48.
    let expected = 0.5 * 0.1 * 0.5 * 4.0 * 48.0 * 48.0;
    let poured = particles(&d);
    let [x, y] = centre(&d);
    eprintln!("poured {poured:.0} particles of {expected:.0}, centre {x:.3}, {y:.3}");
    assert!((poured / expected - 1.0).abs() < 0.1, "{poured} of {expected}");
    // From x = 0.25 * 4/3 heights the stream runs 0.25 heights in half a second: its middle is
    // about 0.125 heights on, at the emitter's height.
    let from = 0.25 * 4.0 / 3.0;
    assert!(x > from + 0.08 && x < from + 0.17, "centre at x {x}, the emitter at {from}");
    assert!((y - 0.5).abs() < 0.02, "centre at y {y}");
    rig.edit(&[set("/tap.on", Value::Bool(false))]);
    let (_, _, before) = run(&mut rig, 2);
    let (out, _, after) = run(&mut rig, 30);
    assert!((particles(&after) - particles(&before)).abs() < 0.02 * poured, "it pours on while off");
    assert!(finite(&out) && finite(&after));
}

/// With open edges a pour leaves at the bottom and the emitter takes the same particles again:
/// the count settles and stays, without a warning, over a long run.
#[test]
fn open_edges_drain_and_the_count_stays_bounded() {
    let Some(gpu) = gpu() else { return };
    let mut ops = vec![
        liquid(&[("edges", "open".into()), ("particles", Value::Int(2048))]),
        create(
            "/tap",
            "tex.liquid_emitter",
            &[("position", Value::Vec2([0.5, 0.05])), ("width", Value::Float(0.08))],
        ),
        wire("/tap", 0, "/liquid", 0),
    ];
    ops.extend(outputs());
    let mut rig = Rig::new(gpu, None, &ops);
    let mut counts = Vec::new();
    for _ in 0..8 {
        let (out, _, d) = run(&mut rig, 120);
        assert!(finite(&out) && finite(&d));
        counts.push(particles(&d));
    }
    eprintln!("in flight every 2 s: {counts:.0?}");
    let late = &counts[2..];
    let (lo, hi) = late.iter().fold((f64::MAX, 0.0f64), |(lo, hi), c| (lo.min(*c), hi.max(*c)));
    assert!(lo > 100.0, "{counts:?}");
    assert!(hi < 1.15 * lo, "the count does not settle: {counts:?}");
    assert!(hi < 2048.0 * 0.5, "{counts:?}");
    assert_eq!(warning(&rig), None);
}

/// A pour into a closed picture uses up its particles: the node warns, the emitter stops adding
/// and nothing breaks. More particles let it pour again.
#[test]
fn a_full_budget_warns_and_stops_the_pour_without_harm() {
    let Some(gpu) = gpu() else { return };
    let mut ops = vec![
        liquid(&[("particles", Value::Int(1024))]),
        create(
            "/tap",
            "tex.liquid_emitter",
            &[
                ("position", Value::Vec2([0.5, 0.2])),
                ("width", Value::Float(0.2)),
                ("speed", Value::Float(1.0)),
            ],
        ),
        wire("/tap", 0, "/liquid", 0),
    ];
    ops.extend(outputs());
    let mut rig = Rig::new(gpu, None, &ops);
    let (out, v, d) = run(&mut rig, 90);
    let w = warning(&rig).unwrap_or_default();
    assert!(w.contains("uses all 1024 particles"), "{w:?}");
    assert!(finite(&out) && finite(&v) && finite(&d));
    let full = particles(&d);
    // The density picture misses what lies against the walls, beyond the picture's last cells.
    assert!(full > 800.0 && full < 1030.0, "{full}");
    let (_, _, d) = run(&mut rig, 30);
    assert!(particles(&d) < 1030.0, "{} after {full}", particles(&d));
    rig.edit(&[set("/liquid.particles", Value::Int(4096))]);
    let (_, _, d) = run(&mut rig, 30);
    assert!(particles(&d) > full + 200.0, "{} after {full}", particles(&d));
    assert_eq!(warning(&rig), None);
}

/// `reset` and `node.reset` put the liquid back where the fill says, as at the first frame.
#[test]
fn reset_restores_the_fill() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("liquid-reset");
    let mut ops = vec![
        liquid(&[]),
        mask("/block", "block.wgsl", &dir, "abs(uv.x - 0.5) < 0.2 && uv.y < 0.4"),
        wire("/block", 0, "/liquid", FILL),
    ];
    ops.extend(outputs());
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    rig.pixels("/block");
    let (first, _, d_first) = run(&mut rig, 1);
    let (second, _, _) = run(&mut rig, 1);
    let (later, _, _) = run(&mut rig, 40);
    assert_ne!(first.data, later.data, "nothing moved");
    rig.edit(&[set("/liquid.reset", Value::Bool(true))]);
    let (again, _, d_again) = run(&mut rig, 1);
    assert_eq!(again.data, first.data);
    assert_eq!(d_again.data, d_first.data);
    // While `reset` is on the liquid stays at the fill.
    let (held, _, _) = run(&mut rig, 3);
    assert_eq!(held.data, first.data);
    // Switched off, it moves on from the fill as at the start, with no second fill on top.
    rig.edit(&[set("/liquid.reset", Value::Bool(false))]);
    let (next, _, _) = run(&mut rig, 1);
    assert_eq!(next.data, second.data);
    run(&mut rig, 20);
    let id = rig.id("/liquid");
    rig.cooker.reset(id);
    let (fresh, _, _) = run(&mut rig, 1);
    assert_eq!(fresh.data, first.data);
}

/// Two runs of a fill and a pour give the same pictures, bit for bit: the fixed-point sums do
/// not depend on the order in which particles add up, nor on which free particle an emitter
/// takes.
#[test]
fn two_runs_are_identical() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("liquid-twice");
    let mut ops = vec![
        liquid(&[]),
        mask("/block", "block.wgsl", &dir, "uv.x > 0.6 && uv.y > 0.5"),
        wire("/block", 0, "/liquid", FILL),
        create("/tap", "tex.liquid_emitter", &[("direction", Value::Vec2([0.3, 1.0]))]),
        wire("/tap", 0, "/liquid", 0),
    ];
    ops.extend(outputs());
    let once = || {
        let mut rig = Rig::new(gpu.clone(), Some(&dir), &ops);
        rig.pixels("/block");
        run(&mut rig, 60)
    };
    let (a, b) = (once(), once());
    assert!(particles(&a.2) > 1000.0);
    assert!(a.0.data == b.0.data, "out differs");
    assert!(a.1.data == b.1.data, "velocity differs");
    assert!(a.2.data == b.2.data, "density differs");
}

/// A stall of ten seconds is one step of at most 1/30 s: nothing flies apart, no particle is
/// lost, and the substeps stay within the cap.
#[test]
fn a_hitch_does_not_blow_up() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("liquid-hitch");
    let mut ops = vec![
        liquid(&[("particles", Value::Int(8192))]),
        mask("/block", "block.wgsl", &dir, "uv.x < 0.5 && uv.y > 0.3"),
        wire("/block", 0, "/liquid", FILL),
    ];
    ops.extend(outputs());
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    rig.pixels("/block");
    let (_, _, d) = run(&mut rig, 20);
    let before = particles(&d);
    rig.clock.advance(600);
    let (out, v, d) = run(&mut rig, 1);
    assert!(finite(&out) && finite(&v) && finite(&d));
    assert!((particles(&d) / before - 1.0).abs() < 0.02, "{} after {before}", particles(&d));
    assert!(mean_speed(&v, &d) < 2.0, "{}", mean_speed(&v, &d));
    assert_eq!(warning(&rig), None);
}

/// A picture full of liquid under a gravity three hundred times the earth's squeezes it to
/// the floor far beyond its rest density: no NaN anywhere, no particle lost, and back under
/// the usual gravity it is a liquid again.
#[test]
fn a_squeeze_makes_no_nan_and_the_liquid_recovers() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("liquid-squeeze");
    let mut ops = vec![
        liquid(&[("grid", Value::Int(32))]),
        mask("/all", "all.wgsl", &dir, "uv.x >= 0.0"),
        wire("/all", 0, "/liquid", FILL),
    ];
    ops.extend(outputs());
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    rig.pixels("/all");
    let (_, _, d) = run(&mut rig, 1);
    let start = particles(&d);
    rig.edit(&[set("/liquid.gravity", Value::Vec2([0.0, 1000.0]))]);
    let mut squeezed = 0.0f32;
    for _ in 0..6 {
        let (out, v, d) = run(&mut rig, 10);
        assert!(finite(&out) && finite(&v) && finite(&d));
        squeezed = squeezed.max(d.stats().max[0]);
    }
    eprintln!("squeeze: {start:.0} particles, densest cell {squeezed:.1} times rest, {:?}", warning(&rig));
    // The fixed-point sums hold the squeeze: no cell's mass wrapped.
    assert_eq!(warning(&rig), None);
    assert!(squeezed > 2.0, "not squeezed: {squeezed}");
    rig.edit(&[set("/liquid.gravity", Value::Vec2([0.0, 3.27]))]);
    let (out, v, d) = run(&mut rig, 120);
    assert!(finite(&out) && finite(&v) && finite(&d));
    let densest = d.stats().max[0];
    assert!(densest < 1.6, "still squeezed: {densest}");
    assert!((particles(&d) / start - 1.0).abs() < 0.1, "{} after {start}", particles(&d));
}

const HOSTILE: &str = "\
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let zero = v.uv.x - v.uv.x;
    let p = vec2i(v.pos.xy);
    if ((p.x + p.y) % 3 == 0) {
        return vec4f(zero / zero, 1.0 / zero, -1.0 / zero, zero / zero);
    }
    return vec4f(1.0, 1.0, 1.0, 1.0);
}
";

/// NaN and infinity in the fill, and emitter tables with absurd numbers, leave every output
/// finite.
#[test]
fn hostile_input_leaves_the_liquid_finite() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("liquid-hostile");
    std::fs::write(dir.join("hostile.wgsl"), HOSTILE).unwrap();
    let rows =
        "x,y,width,vx,vy,on\n1e300,-1e300,1e300,1e300,nan,1\n0.5,0.5,1e9,-1e9,1e9,1\n0.2,0.2,0.1,0,1,yes\n";
    let mut ops = vec![
        liquid(&[("grid", Value::Int(32))]),
        create(
            "/bad",
            "tex.shader",
            &[("file", "hostile.wgsl".into()), ("width", Value::Int(128)), ("height", Value::Int(96))],
        ),
        create("/rows", "table.text", &[("text", rows.into()), ("format", "csv".into())]),
        wire("/bad", 0, "/liquid", FILL),
        wire("/rows", 0, "/liquid", 0),
    ];
    ops.extend(outputs());
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    rig.pixels("/bad");
    let (out, v, d) = run(&mut rig, 30);
    assert!(finite(&out) && finite(&v) && finite(&d));
    assert!(particles(&d) > 100.0, "{}", particles(&d));
}

/// Times the node on the hardware GPU: at the defaults (1920 x 1080, grid 384, 65536 particles,
/// most of them in a pool and a pour), and at the size of the compute fixture's timing (65536
/// particles, 384 cells per picture height, 37 substeps). Prints the wall time per frame waited for, and per frame of
/// frames cooked back to back.
#[test]
#[ignore = "times the liquid on the hardware GPU: cargo test -p penumbra-ops --release liquid_timing -- --ignored --nocapture"]
fn liquid_timing() {
    let gpu = penumbra_gpu::Gpu::headless(penumbra_gpu::AdapterChoice::Hardware).unwrap();
    eprintln!("{}", gpu.name());
    let dir = scratch("liquid-timing");
    for (name, width, height, grid, particles, inside) in [
        ("small", 1920, 1080, 256, 16384, "uv.y > 0.9"),
        ("defaults", 1920, 1080, 384, 65536, "uv.y > 0.82"),
        ("large", 1920, 1080, 512, 131_072, "uv.y > 0.8"),
        ("fixture size", 1440, 1080, 512, 65536, "uv.x < 0.25 && uv.y > 2.0 / 3.0"),
    ] {
        std::fs::write(
            dir.join("fill.wgsl"),
            format!(
                "@fragment\nfn main(v: PnVertex) -> @location(0) vec4f {{\n    let uv = v.uv;\n    return vec4f(vec3f(select(0.0, 1.0, {inside})), 1.0);\n}}\n"
            ),
        )
        .unwrap();
        let ops = [
            create(
                "/liquid",
                "tex.liquid",
                &[
                    ("width", Value::Int(width)),
                    ("height", Value::Int(height)),
                    ("grid", Value::Int(grid)),
                    ("particles", Value::Int(particles)),
                ],
            ),
            create(
                "/fill",
                "tex.shader",
                &[("file", "fill.wgsl".into()), ("width", Value::Int(width)), ("height", Value::Int(height))],
            ),
            wire("/fill", 0, "/liquid", FILL),
            create("/tap", "tex.liquid_emitter", &[]),
            wire("/tap", 0, "/liquid", 0),
        ];
        let mut rig = Rig::new(gpu.clone(), Some(&dir), &ops);
        rig.pixels("/fill");
        let id = rig.id("/liquid");
        let frames = 300;
        let mut wall = Vec::new();
        for f in 0..frames {
            rig.clock.advance(1);
            rig.gpu.poll();
            let t0 = std::time::Instant::now();
            rig.cooker.cook(&rig.net, rig.clock.now(), &[id], &[]);
            rig.gpu.wait(std::time::Duration::from_secs(10)).unwrap();
            if f >= 20 {
                wall.push(t0.elapsed().as_secs_f64() * 1e3);
            }
        }
        wall.sort_by(f64::total_cmp);
        let mean = wall.iter().sum::<f64>() / wall.len() as f64;
        let p95 = wall[wall.len() * 95 / 100];
        let t0 = std::time::Instant::now();
        for _ in 0..frames {
            rig.clock.advance(1);
            rig.gpu.poll();
            rig.cooker.cook(&rig.net, rig.clock.now(), &[id], &[]);
        }
        rig.gpu.wait(std::time::Duration::from_secs(30)).unwrap();
        let throughput = t0.elapsed().as_secs_f64() * 1e3 / frames as f64;
        let status = rig.cooker.status(id).unwrap();
        eprintln!(
            "{name}: {width}x{height}, grid {grid}, {particles} particles: waited for, ms per frame mean {mean:.3} p95 {p95:.3}; back to back \
             {throughput:.3} ms per frame; cook on the CPU mean {:.3} ms; {} MB on the GPU; warning {:?}",
            status.stats.average.as_secs_f64() * 1e3,
            status.gpu_bytes / 1_000_000,
            status.warning
        );
    }
}
