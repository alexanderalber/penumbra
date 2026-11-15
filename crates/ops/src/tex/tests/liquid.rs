//! `tex.liquid` and `tex.liquid_emitter` on WARP: a block falls into a flat pool of the right
//! height, emitters pour where and as much as they say, open edges drain, a full budget warns,
//! reset, repeatability, a hitch, a squeeze and hostile input. Obstacles: a shelf holds a pool,
//! an obstacle that appears in the liquid pushes it out, a moving one with its velocity pushes
//! it along, friction holds liquid on a slope, an empty mask changes nothing. Sand and snow:
//! poured sand heaps at its angle and rests, dropped snow keeps a pile, all three materials in
//! one node, sand on a shelf and snow on a slope, hostile input.

use penumbra_core::Value;
use penumbra_core::scratch::ScratchDir;
use penumbra_gpu::Pixels;
use penumbra_graph::{MAX_RUN, Op};

use super::{Rig, create, gpu, scratch, set};

/// The `fill` input, after the run of emitters, then `obstacles` and `obstacle_velocity`.
const FILL: u32 = MAX_RUN as u32;
const OBSTACLES: u32 = FILL + 1;
const OBSTACLE_VELOCITY: u32 = FILL + 2;

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
        // Keeps the software adapter's queue short: the tests share it, and a long queue of
        // sand or snow makes another test's read time out.
        if f % 5 == 4 {
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
    if let Some(e) = rig.cooker.status(rig.id("/liquid")).and_then(|s| s.error) {
        panic!("{e}");
    }
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
    depth_on(d, i, 1.0)
}

/// The depth of the liquid in column `i` that rests on a floor at `floor` picture heights.
fn depth_on(d: &Pixels, i: u32, floor: f64) -> f64 {
    let dx = 1.0 / f64::from(d.height);
    let at = |j: u32| f64::from(d.get(i, j).unwrap()[0]);
    let rows = ((floor / dx).ceil() as u32).min(d.height);
    for j in (1..rows).rev() {
        let (below, above) = (at(j), at(j - 1));
        if below >= 0.5 && above < 0.5 {
            // Between the centres of cells j and j - 1.
            let t = (below - 0.5) / (below - above);
            let y = (f64::from(j) + 0.5 - t) * dx;
            return floor - y;
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

/// Particles in the cells whose centre (in picture heights from the top left) `inside` takes.
fn particles_where(d: &Pixels, inside: impl Fn(f64, f64) -> bool) -> f64 {
    let dx = 1.0 / f64::from(d.height);
    let mut sum = 0.0;
    for j in 0..d.height {
        for i in 0..d.width {
            if inside((f64::from(i) + 0.5) * dx, (f64::from(j) + 0.5) * dx) {
                sum += f64::from(d.get(i, j).unwrap()[0]);
            }
        }
    }
    sum * 4.0
}

/// A picture of 128 x 96 from a fragment shader body: `velocity` (a vec2f expression of `uv` and
/// `t`, the show seconds) where `inside` holds, black elsewhere.
fn moving(path: &str, file: &str, dir: &std::path::Path, inside: &str, velocity: &str) -> Op {
    let source = format!(
        "@fragment\nfn main(v: PnVertex) -> @location(0) vec4f {{\n    let uv = v.uv;\n    let t = pn_time.seconds;\n    \
         return select(vec4f(0.0, 0.0, 0.0, 1.0), vec4f({velocity}, 0.0, 1.0), {inside});\n}}\n"
    );
    std::fs::write(dir.join(file), source).unwrap();
    create(
        path,
        "tex.shader",
        &[("file", file.into()), ("width", Value::Int(128)), ("height", Value::Int(96))],
    )
}

/// A shelf across the whole picture, 0.1 heights thick at half its height, holds the liquid
/// filled above it: nothing below it, a flat pool on it as deep as the fill over the width, no
/// particle lost. The fill reaches into the shelf, and no liquid starts there.
#[test]
fn a_shelf_holds_a_pool_and_nothing_passes_through() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("liquid-shelf");
    let mut ops = vec![
        liquid(&[("particles", Value::Int(8192))]),
        mask("/block", "block.wgsl", &dir, "abs(uv.x - 0.5) < 0.2 && uv.y > 0.2 && uv.y < 0.55"),
        mask("/shelf", "shelf.wgsl", &dir, "uv.y > 0.5 && uv.y < 0.6"),
        wire("/block", 0, "/liquid", FILL),
        wire("/shelf", 0, "/liquid", OBSTACLES),
    ];
    ops.extend(outputs());
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    rig.pixels("/block");
    rig.pixels("/shelf");
    let (_, _, d0) = run(&mut rig, 1);
    let start = particles(&d0);
    // The fill stops at the shelf: 0.4 widths by 0.3 heights.
    let filled = 0.4 * (4.0 / 3.0) * 0.3 * 4.0 * 48.0 * 48.0;
    assert!((start / filled - 1.0).abs() < 0.05, "{start} particles at the start, {filled} expected");
    let (out, v, d) = run(&mut rig, 360);
    assert!(finite(&out) && finite(&v) && finite(&d));
    let below = particles_where(&d, |_, y| y > 0.62);
    // The cells of the shelf's top row hold the pool's bottom.
    let inside = particles_where(&d, |_, y| y > 0.53 && y < 0.58);
    let on = particles_where(&d, |_, y| y < 0.53);
    // The pool on the shelf: as deep as the fill's area over the width, its top at 0.5 less that.
    let expected = 0.4 * 0.3;
    let tops: Vec<f64> = (8..56).map(|i| depth_on(&d, i, 0.5)).collect();
    let mean = tops.iter().sum::<f64>() / tops.len() as f64;
    eprintln!(
        "shelf: {start:.0} particles, then {on:.0} above, {inside:.0} inside, {below:.0} below; pool {mean:.4} \
         heights deep (expected {expected:.4}), speed {:.4}",
        mean_speed(&v, &d)
    );
    assert!(below < 1.0, "{below} particles below the shelf");
    assert!(inside < 0.02 * start, "{inside} particles inside the shelf");
    assert!((on / start - 1.0).abs() < 0.05, "{on} of {start} on the shelf");
    assert!((mean / expected - 1.0).abs() < 0.1, "pool {mean} against {expected}: {tops:?}");
    assert_eq!(warning(&rig), None);
}

/// A box that appears in a pool pushes the liquid out of itself within a second or two: the
/// particles come out along the distance field, none is lost and none flies off. Particles inside
/// a solid add nothing to the density picture, so its count drops while they are in there.
#[test]
fn an_obstacle_that_appears_in_the_liquid_pushes_it_out() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("liquid-appear");
    let mut ops = vec![
        liquid(&[("particles", Value::Int(16384))]),
        mask("/pool", "pool.wgsl", &dir, "uv.y > 0.5"),
        mask("/box", "box.wgsl", &dir, "abs(uv.x - 0.5) < 0.15 && uv.y > 0.7 && uv.y < 0.9"),
        wire("/pool", 0, "/liquid", FILL),
    ];
    ops.extend(outputs());
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    rig.pixels("/pool");
    rig.pixels("/box");
    let (_, _, d) = run(&mut rig, 30);
    let start = particles(&d);
    rig.edit(&[wire("/box", 0, "/liquid", OBSTACLES)]);
    // The box in heights: x from 0.35 to 0.65 of 4/3, y from 0.7 to 0.9; a cell less all round.
    let in_box = |x: f64, y: f64| (x / (4.0 / 3.0) - 0.5).abs() < 0.15 - 0.03 && y > 0.72 && y < 0.88;
    let (_, _, d) = run(&mut rig, 2);
    let early = particles(&d);
    let (out, v, d) = run(&mut rig, 150);
    assert!(finite(&out) && finite(&v) && finite(&d));
    let late = particles_where(&d, in_box);
    let total = particles(&d);
    eprintln!(
        "appear: {start:.0} particles; {early:.0} outside the box at once, {total:.0} after 2.5 s, {late:.1} in \
         the box; speed {:.4}",
        mean_speed(&v, &d)
    );
    // The box covers 0.08 heights squared of the pool: about 740 particles.
    assert!(early < start - 400.0, "{early} of {start} outside the box at once");
    assert!((total / start - 1.0).abs() < 0.01, "{total} after {start}");
    assert!(late < 0.005 * start, "{late} particles left in the box");
    assert!(mean_speed(&v, &d) < 0.1, "{}", mean_speed(&v, &d));
}

/// A paddle that moves through a pool pushes the liquid ahead of it when `obstacle_velocity`
/// says how it moves. Without the velocity the paddle only takes the liquid's place, and less
/// of it ends up ahead.
#[test]
fn a_moving_obstacle_with_its_velocity_pushes_the_liquid() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("liquid-paddle");
    // From x 0.1 to 0.6 of the width in 1.5 s, 0.05 wide, from 0.4 down to the floor.
    let paddle = "abs(uv.x - (0.1 + t / 3.0)) < 0.025 && uv.y > 0.4";
    let pushes = |with_velocity: bool| {
        let mut ops = vec![
            liquid(&[("particles", Value::Int(16384))]),
            mask("/pool", "pool.wgsl", &dir, "uv.y > 0.7"),
            moving("/paddle", "paddle.wgsl", &dir, paddle, "vec2f(1.0, 1.0)"),
            moving("/push", "push.wgsl", &dir, paddle, "vec2f(1.0 / 3.0, 0.0)"),
            wire("/pool", 0, "/liquid", FILL),
            wire("/paddle", 0, "/liquid", OBSTACLES),
        ];
        if with_velocity {
            ops.push(wire("/push", 0, "/liquid", OBSTACLE_VELOCITY));
        }
        ops.extend(outputs());
        let mut rig = Rig::new(gpu.clone(), Some(&dir), &ops);
        rig.pixels("/pool");
        rig.pixels("/paddle");
        rig.pixels("/push");
        let (_, _, d) = run(&mut rig, 1);
        let start = particles(&d);
        let (out, v, d) = run(&mut rig, 80);
        assert!(finite(&out) && finite(&v) && finite(&d));
        // Liquid ahead of the paddle's place at the end (x 0.6 of the width = 0.8 heights).
        let ahead = particles_where(&d, |x, _| x > 0.8 + 0.035);
        (start, ahead, centre(&d)[0], particles(&d))
    };
    let (start, ahead, x, total) = pushes(true);
    let (_, ahead_still, x_still, _) = pushes(false);
    eprintln!(
        "paddle: {start:.0} particles; with its velocity {ahead:.0} ahead and centre {x:.3}, {total:.0} in all; \
         without {ahead_still:.0} ahead and centre {x_still:.3}"
    );
    // The pool at the start: 0.3 deep everywhere, so 40 % of it lies beyond x 0.6.
    assert!(ahead > 0.5 * start, "{ahead} of {start} ahead");
    assert!(ahead > ahead_still + 0.05 * start, "{ahead} with velocity, {ahead_still} without");
    assert!(x > x_still, "{x} {x_still}");
}

/// A film of liquid two cells thick on a slope of 24 degrees: without friction it runs down to
/// the bottom corner; with `friction` 1 it stays near where it was.
#[test]
fn friction_holds_liquid_on_a_slope() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("liquid-slope");
    let slide = |friction: f64| {
        let mut ops = vec![
            liquid(&[("friction", Value::Float(friction))]),
            mask("/drop", "drop.wgsl", &dir, "uv.x > 0.05 && uv.x < 0.4 && uv.y > 0.26 + 0.6 * uv.x"),
            mask("/slope", "slope.wgsl", &dir, "uv.y > 0.3 + 0.6 * uv.x"),
            wire("/drop", 0, "/liquid", FILL),
            wire("/slope", 0, "/liquid", OBSTACLES),
        ];
        ops.extend(outputs());
        let mut rig = Rig::new(gpu.clone(), Some(&dir), &ops);
        rig.pixels("/drop");
        rig.pixels("/slope");
        let (out, v, d) = run(&mut rig, 60);
        assert!(finite(&out) && finite(&v) && finite(&d));
        (centre(&d)[0], particles(&d))
    };
    let (free, n_free) = slide(0.0);
    let (held, n_held) = slide(1.0);
    eprintln!(
        "slope after 1 s: centre x {free:.3} without friction ({n_free:.0}), {held:.3} with 1 ({n_held:.0})"
    );
    // The film starts with its centre at 0.3 heights.
    assert!(free > 0.65, "{free}");
    assert!(held < 0.45 && held < free - 0.25, "{held} against {free}");
}

/// A mask that changes inside the same texture still moves the obstacle: `tex.cache` of length
/// 1 copies every new frame into the one texture it outputs. The mask is empty for the first
/// half second, then a box appears in the pool, and the liquid leaves the box.
#[test]
fn a_mask_rewritten_in_the_same_texture_is_seen() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("liquid-same-texture");
    let mut ops = vec![
        liquid(&[("particles", Value::Int(16384))]),
        mask("/pool", "pool.wgsl", &dir, "uv.y > 0.5"),
        moving(
            "/box",
            "box.wgsl",
            &dir,
            "t > 0.5 && abs(uv.x - 0.5) < 0.15 && uv.y > 0.7 && uv.y < 0.9",
            "vec2f(1.0, 1.0)",
        ),
        create("/held", "tex.cache", &[("length", Value::Int(1))]),
        wire("/box", 0, "/held", 0),
        wire("/pool", 0, "/liquid", FILL),
        wire("/held", 0, "/liquid", OBSTACLES),
    ];
    ops.extend(outputs());
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    rig.pixels("/pool");
    rig.pixels("/box");
    let (_, _, d) = run(&mut rig, 2);
    let first = rig.tex("/held").unwrap();
    let start = particles(&d);
    let (out, _, d) = run(&mut rig, 150);
    assert!(finite(&out) && finite(&d));
    // The fixture: the cache still outputs the texture of the first frames.
    assert!(rig.tex("/held").unwrap().same(&first), "tex.cache made a new texture");
    let in_box = |x: f64, y: f64| (x / (4.0 / 3.0) - 0.5).abs() < 0.15 - 0.03 && y > 0.72 && y < 0.88;
    let left = particles_where(&d, in_box);
    eprintln!("same texture: {start:.0} particles, {left:.1} left in the box after 2.5 s");
    assert!(left < 0.005 * start, "{left} particles in the box: the obstacle was not seen");
}

/// An all-black `obstacles` picture gives the same liquid as none: the edges flooded on the GPU
/// are the edges the CPU writes.
#[test]
fn an_empty_obstacle_picture_changes_nothing() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("liquid-empty-obstacles");
    let once = |wired: bool| {
        let mut ops = vec![
            liquid(&[]),
            mask("/block", "block.wgsl", &dir, "uv.x < 0.4 && uv.y > 0.3"),
            mask("/none", "none.wgsl", &dir, "uv.x < 0.0"),
            wire("/block", 0, "/liquid", FILL),
        ];
        if wired {
            ops.push(wire("/none", 0, "/liquid", OBSTACLES));
        }
        ops.extend(outputs());
        let mut rig = Rig::new(gpu.clone(), Some(&dir), &ops);
        rig.pixels("/block");
        rig.pixels("/none");
        run(&mut rig, 120)
    };
    let (a, b) = (once(false), once(true));
    let (ca, cb) = (centre(&a.2), centre(&b.2));
    eprintln!(
        "empty obstacles: centre {ca:?} against {cb:?}, identical: {}",
        a.0.data == b.0.data && a.2.data == b.2.data
    );
    assert!((particles(&a.2) - particles(&b.2)).abs() < 0.01 * particles(&a.2));
    assert!((ca[0] - cb[0]).abs() < 0.01 && (ca[1] - cb[1]).abs() < 0.01, "{ca:?} {cb:?}");
}

const HOSTILE_OBSTACLE: &str = "\
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let zero = v.uv.x - v.uv.x;
    let p = vec2i(v.pos.xy);
    if ((p.x + 2 * p.y) % 5 == 0) {
        return vec4f(zero / zero, 1.0 / zero, -1.0 / zero, zero / zero);
    }
    if (v.uv.x > 0.6 && v.uv.y > 0.3) {
        return vec4f(1e30, -1e30, 1.0, 1.0);
    }
    return vec4f(0.0, 0.0, 0.0, 1.0);
}
";

/// NaN and infinity in the obstacles and their velocity leave every output finite.
#[test]
fn hostile_obstacles_leave_the_liquid_finite() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("liquid-hostile-obstacles");
    std::fs::write(dir.join("hostile.wgsl"), HOSTILE_OBSTACLE).unwrap();
    let mut ops = vec![
        liquid(&[("grid", Value::Int(32))]),
        create(
            "/bad",
            "tex.shader",
            &[("file", "hostile.wgsl".into()), ("width", Value::Int(128)), ("height", Value::Int(96))],
        ),
        mask("/block", "block.wgsl", &dir, "uv.y > 0.2"),
        wire("/block", 0, "/liquid", FILL),
        wire("/bad", 0, "/liquid", OBSTACLES),
        wire("/bad", 0, "/liquid", OBSTACLE_VELOCITY),
    ];
    ops.extend(outputs());
    ops.extend(drawn());
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    rig.pixels("/bad");
    rig.pixels("/block");
    let all = run_drawn(&mut rig, 60);
    assert!(all.iter().all(finite));
    assert!(particles(&all[2]) > 100.0, "{}", particles(&all[2]));
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
    ops.extend(drawn());
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    rig.pixels("/bad");
    let all = run_drawn(&mut rig, 30);
    assert!(all.iter().all(finite));
    assert!(particles(&all[2]) > 100.0, "{}", particles(&all[2]));
}

/// The drawn outputs read through nulls: `/f` foam, `/g` grain, `/s` surface.
fn drawn() -> [Op; 6] {
    [
        create("/f", "tex.null", &[]),
        create("/g", "tex.null", &[]),
        create("/s", "tex.null", &[]),
        wire("/liquid", 3, "/f", 0),
        wire("/liquid", 4, "/g", 0),
        wire("/liquid", 5, "/s", 0),
    ]
}

/// Moves the show on `frames` frames cooking the liquid and every null, and reads `/liquid`,
/// `/v`, `/d`, `/f`, `/g` and `/s`.
fn run_drawn(rig: &mut Rig, frames: usize) -> [Pixels; 6] {
    let paths = ["/liquid", "/v", "/d", "/f", "/g", "/s"];
    for f in 0..frames {
        if f % 5 == 4 {
            rig.gpu.wait(std::time::Duration::from_secs(120)).unwrap();
        }
        rig.clock.advance(1);
        rig.gpu.poll();
        let ids: Vec<_> = paths.iter().map(|p| rig.id(p)).collect();
        rig.cooker.cook(&rig.net, rig.clock.now(), &ids, &[]);
    }
    if let Some(e) = rig.cooker.status(rig.id("/liquid")).and_then(|s| s.error) {
        panic!("{e}");
    }
    paths.map(|p| rig.gpu.read(&rig.tex(p).unwrap()).unwrap())
}

/// Sum of the foam picture's alpha: how much foam there is, in pixels.
fn foam(f: &Pixels) -> f64 {
    f.data.iter().map(|c| f64::from(c[3])).sum()
}

/// Pixels the liquid covers in `out`.
fn covered(out: &Pixels) -> f64 {
    out.data.iter().map(|c| f64::from(c[3])).sum()
}

/// A pool in the lower half: every output at its size, in the format between nodes. `out` is
/// transparent black away from the liquid and opaque inside the pool, also against the walls;
/// foam and grain lie within it; the velocity's blue is its speed.
#[test]
fn the_drawing_has_every_output_at_its_size_and_a_clean_alpha() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("liquid-drawn");
    let mut ops =
        vec![liquid(&[]), mask("/pool", "pool.wgsl", &dir, "uv.y > 0.6"), wire("/pool", 0, "/liquid", FILL)];
    ops.extend(outputs());
    ops.extend(drawn());
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    rig.pixels("/pool");
    let [out, v, d, f, g, s] = run_drawn(&mut rig, 120);
    for (name, px, size) in [
        ("out", &out, (128, 96)),
        ("velocity", &v, (64, 48)),
        ("density", &d, (64, 48)),
        ("foam", &f, (128, 96)),
        ("grain", &g, (128, 96)),
        ("surface", &s, (128, 96)),
    ] {
        assert_eq!((px.width, px.height), size, "{name}");
        assert!(finite(px), "{name}");
    }
    for p in ["/liquid", "/f", "/g", "/s", "/v", "/d"] {
        assert_eq!(rig.tex(p).unwrap().format(), penumbra_gpu::FORMAT, "{p}");
    }
    // Away from the pool: nothing at all. Its top settles at 0.6 of the height.
    for j in 0..48 {
        for i in 0..128 {
            assert_eq!(out.get(i, j).unwrap(), [0.0; 4], "out at {i}, {j}");
        }
    }
    // Inside, from wall to wall: opaque.
    for j in 70..96 {
        for i in 0..128 {
            let a = out.get(i, j).unwrap()[3];
            assert!(a > 0.999, "alpha {a} at {i}, {j}");
        }
    }
    // The edge between is antialiased: a few pixels between 0 and 1 per column, not a band.
    let soft =
        (0..128).map(|i| (0..96).filter(|j| (0.02..0.98).contains(&out.get(i, *j).unwrap()[3])).count());
    let most = soft.max().unwrap();
    assert!(most <= 3, "{most} soft pixels in a column");
    // Foam and grain lie within the liquid, at most as opaque as it.
    for ((o, fo), gr) in out.data.iter().zip(&f.data).zip(&g.data) {
        assert!(fo[3] <= o[3] + 1e-3 && gr[3] <= o[3] + 1e-3 && gr[0] <= gr[3] + 1e-3);
    }
    // The grain is a random grey: about half on average inside.
    let inside: Vec<f64> = g.data.iter().filter(|c| c[3] > 0.999).map(|c| f64::from(c[0])).collect();
    let mean = inside.iter().sum::<f64>() / inside.len() as f64;
    assert!((mean - 0.5).abs() < 0.1, "grain {mean}");
    // The velocity's blue is the speed, in heights per second; alpha how full the cell is.
    for c in &v.data {
        let speed = (f64::from(c[0]) * 4.0 / 3.0).hypot(f64::from(c[1]));
        assert!((f64::from(c[2]) - speed).abs() < 0.01 + 0.01 * speed, "{c:?}");
        assert!((0.0..=1.0).contains(&c[3]), "{c:?}");
    }
    // The surface: positive distance deep in the pool, negative above it.
    assert!(s.get(64, 90).unwrap()[0] > 3.0 && s.get(64, 10).unwrap()[0] < -3.0);
}

/// A block of liquid that falls into a pool.
fn splash(name: &str) -> (ScratchDir, Vec<Op>) {
    let dir = scratch(name);
    let mut ops = vec![
        liquid(&[]),
        mask(
            "/block",
            "block.wgsl",
            &dir,
            "(abs(uv.x - 0.5) < 0.1 && uv.y > 0.15 && uv.y < 0.45) || uv.y > 0.75",
        ),
        wire("/block", 0, "/liquid", FILL),
    ];
    ops.extend(outputs());
    ops.extend(drawn());
    (dir, ops)
}

/// Foam starts where a falling block lands in a pool, and is gone once the pool calmed down.
#[test]
fn foam_appears_in_a_splash_and_fades_in_a_calm_pool() {
    let Some(gpu) = gpu() else { return };
    let (dir, ops) = splash("liquid-foam");
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    rig.pixels("/block");
    let [first, _, _, f0, _, _] = run_drawn(&mut rig, 1);
    let [_, _, _, landed, _, _] = run_drawn(&mut rig, 30);
    let [_, _, _, calm, _, _] = run_drawn(&mut rig, 570);
    let most = |f: &Pixels| f.data.iter().fold(0.0f32, |m, c| m.max(c[3]));
    eprintln!(
        "foam: {:.0} at the start, {:.0} (most {:.2}) as it lands, {:.1} (most {:.3}) after 10 s",
        foam(&f0),
        foam(&landed),
        most(&landed),
        foam(&calm),
        most(&calm)
    );
    assert!(covered(&first) > 1000.0);
    assert_eq!(foam(&f0), 0.0, "foam before anything moved");
    assert!(foam(&landed) > 100.0 && most(&landed) > 0.5, "{}", foam(&landed));
    assert!(most(&calm) < 0.05, "{}", most(&calm));
}

/// The same splash at 30, 60 and 144 frames per second: after the same show time the foam and
/// the covered area are nearly the same. The smoothing and fading follow the time a substep
/// stands for, not the number of frames. (At 30 and 60 the substeps happen to be the same, so
/// those two agree exactly; 144 takes substeps of another length.)
#[test]
fn the_look_does_not_depend_on_the_frame_rate() {
    let Some(gpu) = gpu() else { return };
    let (dir, ops) = splash("liquid-rate");
    let at = |fps: u32| {
        let mut rig = Rig::new(gpu.clone(), Some(&dir), &ops);
        rig.clock.set_rate(penumbra_core::Rate::new(fps, 1).unwrap());
        rig.pixels("/block");
        let [out_a, _, _, foam_a, _, _] = run_drawn(&mut rig, fps as usize / 2);
        let [out_b, _, _, foam_b, _, _] = run_drawn(&mut rig, fps as usize);
        [(covered(&out_a), foam(&foam_a)), (covered(&out_b), foam(&foam_b))]
    };
    let (thirty, sixty, fast) = (at(30), at(60), at(144));
    eprintln!("30 fps: {thirty:?}; 60 fps: {sixty:?}; 144 fps: {fast:?} (area, foam at 0.5 s and 1.5 s)");
    for other in [thirty, fast] {
        for ((area, foam), (area_60, foam_60)) in other.into_iter().zip(sixty) {
            assert!((area / area_60 - 1.0).abs() < 0.03, "area {area}, {area_60} at 60 fps");
            assert!(foam_60 > 20.0, "{foam_60}");
            assert!((foam / foam_60 - 1.0).abs() < 0.25, "foam {foam}, {foam_60} at 60 fps");
        }
    }
}

/// The top of the liquid in each column of the density picture, picture heights above the floor.
fn profile(d: &Pixels) -> Vec<f64> {
    (0..d.width).map(|i| depth(d, i)).collect()
}

/// The slopes of a heap's two flanks in degrees, from a `profile` with cells `dx` wide: lines
/// fitted to the columns between a fifth and four fifths of the peak, either side of it.
fn flanks(tops: &[f64], dx: f64) -> (f64, f64) {
    let (peak_at, peak) =
        tops.iter().enumerate().fold((0, 0.0f64), |m, (i, t)| if *t > m.1 { (i, *t) } else { m });
    let fit = |range: &mut dyn Iterator<Item = usize>| {
        let pts: Vec<(f64, f64)> = range
            .filter(|i| tops[*i] > 0.2 * peak && tops[*i] < 0.8 * peak)
            .map(|i| (i as f64 * dx, tops[i]))
            .collect();
        if pts.len() < 3 {
            return f64::NAN;
        }
        let n = pts.len() as f64;
        let (mx, my) = (pts.iter().map(|p| p.0).sum::<f64>() / n, pts.iter().map(|p| p.1).sum::<f64>() / n);
        let sxy: f64 = pts.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
        let sxx: f64 = pts.iter().map(|p| (p.0 - mx) * (p.0 - mx)).sum();
        (sxy / sxx).abs().atan().to_degrees()
    };
    (fit(&mut (0..peak_at)), fit(&mut (peak_at + 1..tops.len())))
}

/// Opaque pixels of `out` (premultiplied) in the default colours of sand, liquid and snow.
fn kinds(out: &Pixels) -> [usize; 3] {
    let mut n = [0; 3];
    for c in &out.data {
        if c[3] < 0.9 {
            continue;
        }
        let (r, g, b) = (c[0], c[1], c[2]);
        if r > 1.6 * b && r > 0.15 {
            n[0] += 1;
        } else if b > 2.0 * r {
            n[1] += 1;
        } else if r > 0.3 && g > 0.3 && b > 0.3 && (b - r).abs() < 0.3 {
            n[2] += 1;
        }
    }
    n
}

/// Sand poured for two seconds forms a heap whose flanks stand at about `sand_angle`, keeps
/// the area its particles stand for, and then stays as it is.
#[test]
fn poured_sand_forms_a_heap_at_its_angle_and_stays_at_rest() {
    let Some(gpu) = gpu() else { return };
    let mut ops = vec![
        liquid(&[("grid", Value::Int(64)), ("particles", Value::Int(4096))]),
        create(
            "/tap",
            "tex.liquid_emitter",
            &[
                ("position", Value::Vec2([0.5, 0.1])),
                ("width", Value::Float(0.04)),
                ("speed", Value::Float(1.0)),
                ("material", "sand".into()),
            ],
        ),
        wire("/tap", 0, "/liquid", 0),
    ];
    ops.extend(outputs());
    ops.extend(drawn());
    let mut rig = Rig::new(gpu, None, &ops);
    run_drawn(&mut rig, 120);
    rig.edit(&[set("/tap.on", Value::Bool(false))]);
    let [out, v, d, _, _, _] = run_drawn(&mut rig, 150);
    let [later_out, later_v, later, _, _, _] = run_drawn(&mut rig, 60);
    assert!([&out, &v, &d, &later_out, &later_v, &later].iter().all(|p| finite(p)));
    let dx = 1.0 / f64::from(d.height);
    let (tops, again) = (profile(&d), profile(&later));
    let (left, right) = flanks(&tops, dx);
    let peak = tops.iter().fold(0.0f64, |a, b| a.max(*b));
    // How far the top moved, on average over the columns: a grain rolling down one column is no
    // landslide.
    let moved = tops.iter().zip(&again).map(|(a, b)| (a - b).abs()).sum::<f64>() / tops.len() as f64;
    // The area the particles stand for against the area of the cells at least half full.
    let held = particles(&later) / 4.0;
    let covered = later.data.iter().filter(|c| c[0] >= 0.5).count() as f64;
    let [sand, water, snow] = kinds(&later_out);
    eprintln!(
        "sand heap: peak {peak:.3}, flanks {left:.1} and {right:.1} degrees, moved {moved:.4} in 1 s, speed {:.4}; \
         {held:.0} cells of particles cover {covered:.0}; pixels sand {sand} water {water} snow {snow}",
        mean_speed(&later_v, &later)
    );
    // Two seconds at 0.04 x 1 heights^2 per second: 0.08. A pool of it would lie 0.06 deep.
    assert!(peak > 0.15, "{peak}");
    for flank in [left, right] {
        assert!((28.0..42.0).contains(&flank), "flanks {left} {right}");
    }
    assert!(moved < 0.2 * dx, "the heap still moves: {moved}");
    assert!(mean_speed(&later_v, &later) < 0.01);
    // The sand is not pressed together: it covers about the cells its particles fill.
    assert!(covered > 0.9 * held, "{covered} cells covered by {held} cells of particles");
    assert!(sand > 500 && water == 0 && snow < sand / 10, "{sand} {water} {snow}");
    assert_eq!(warning(&rig), None);
}

/// A block of snow dropped on the floor keeps a pile of lumps, far higher and narrower than the
/// flat pool the same block of liquid makes.
#[test]
fn dropped_snow_keeps_a_pile() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("liquid-snow-drop");
    let drop = |material: &str| {
        let mut ops = vec![
            liquid(&[("particles", Value::Int(4096)), ("fill_material", material.into())]),
            mask("/block", "block.wgsl", &dir, "abs(uv.x - 0.5) < 0.1 && uv.y > 0.3 && uv.y < 0.6"),
            wire("/block", 0, "/liquid", FILL),
        ];
        ops.extend(outputs());
        ops.extend(drawn());
        let mut rig = Rig::new(gpu.clone(), Some(&dir), &ops);
        rig.pixels("/block");
        let [out, v, d, _, _, _] = run_drawn(&mut rig, 120);
        assert!(finite(&out) && finite(&v) && finite(&d));
        let tops = profile(&d);
        let peak = tops.iter().fold(0.0f64, |a, b| a.max(*b));
        let wide = tops.iter().filter(|t| **t > 0.02).count() as f64 / f64::from(d.width);
        (peak, wide, kinds(&out), warning(&rig))
    };
    let (snow_peak, snow_wide, snow_kinds, snow_warning) = drop("snow");
    eprintln!("dropped: snow {snow_peak:.3} high over {snow_wide:.2} of the width ({snow_kinds:?})");
    // The block as a flat pool would lie 0.06 deep over the whole floor (the dam test shows
    // liquid does that).
    assert!(snow_peak > 0.12, "{snow_peak}");
    assert!(snow_wide < 0.8, "{snow_wide}");
    assert!(snow_kinds[2] > 100 && snow_kinds[0] == 0 && snow_kinds[1] == 0, "{snow_kinds:?}");
    assert_eq!(snow_warning, None);
}

/// A sand mound from the fill, water poured onto it and snow poured beside it: everything stays
/// finite, and each material shows in `out` in its own colour, so the fill's and the emitters'
/// materials reach the particles.
#[test]
fn water_sand_and_snow_in_one_node_stay_finite_and_all_show() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("liquid-water-sand");
    let mut ops = vec![
        liquid(&[("particles", Value::Int(8192)), ("fill_material", "sand".into())]),
        mask("/mound", "mound.wgsl", &dir, "uv.y > 0.75 + abs(uv.x - 0.5)"),
        wire("/mound", 0, "/liquid", FILL),
        create(
            "/tap",
            "tex.liquid_emitter",
            &[
                ("position", Value::Vec2([0.55, 0.1])),
                ("width", Value::Float(0.06)),
                ("speed", Value::Float(1.0)),
            ],
        ),
        create(
            "/snow",
            "tex.liquid_emitter",
            &[
                ("position", Value::Vec2([0.15, 0.1])),
                ("width", Value::Float(0.06)),
                ("material", "snow".into()),
            ],
        ),
        wire("/tap", 0, "/liquid", 0),
        wire("/snow", 0, "/liquid", 1),
    ];
    ops.extend(outputs());
    ops.extend(drawn());
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    rig.pixels("/mound");
    let [first, ..] = run_drawn(&mut rig, 1);
    let all = run_drawn(&mut rig, 100);
    assert!(all.iter().all(finite));
    let (before, after) = (kinds(&first), kinds(&all[0]));
    eprintln!("water on sand: pixels (sand, water, snow) {before:?} at the start, {after:?} after 1.7 s");
    assert!(before[0] > 300 && before[1] == 0 && before[2] == 0, "{before:?}");
    assert!(after[0] > 200 && after[1] > 200 && after[2] > 50, "{after:?}");
    assert_eq!(warning(&rig), None);
}

/// Sand poured onto a shelf heaps up there by its own friction, with `friction` at 0. Snow on a
/// slope stays where liquid runs down.
#[test]
fn sand_stays_on_a_shelf_and_snow_on_a_slope() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("liquid-sand-shelf");
    let mut ops = vec![
        liquid(&[("particles", Value::Int(8192))]),
        mask("/shelf", "shelf.wgsl", &dir, "abs(uv.x - 0.5) < 0.2 && uv.y > 0.5 && uv.y < 0.55"),
        create(
            "/tap",
            "tex.liquid_emitter",
            &[
                ("position", Value::Vec2([0.5, 0.1])),
                ("width", Value::Float(0.04)),
                ("material", "sand".into()),
            ],
        ),
        wire("/shelf", 0, "/liquid", OBSTACLES),
        wire("/tap", 0, "/liquid", 0),
    ];
    ops.extend(outputs());
    let mut rig = Rig::new(gpu.clone(), Some(&dir), &ops);
    rig.pixels("/shelf");
    let (_, _, d) = run(&mut rig, 120);
    rig.edit(&[set("/tap.on", Value::Bool(false))]);
    let (out, v, d_end) = run(&mut rig, 60);
    assert!(finite(&out) && finite(&v) && finite(&d_end));
    let poured = particles(&d);
    let on = particles_where(&d_end, |_, y| y < 0.52);
    let below = particles_where(&d_end, |_, y| y > 0.56);
    eprintln!("sand shelf: {poured:.0} poured, {on:.0} on the shelf, {below:.0} below after 1 s more");
    assert!(on > 0.8 * poured, "{on} of {poured} on the shelf");

    let slide = |material: &str| {
        let mut ops = vec![
            liquid(&[("fill_material", material.into())]),
            mask("/drop", "drop.wgsl", &dir, "uv.x > 0.05 && uv.x < 0.4 && uv.y > 0.22 + 0.6 * uv.x"),
            mask("/slope", "slope.wgsl", &dir, "uv.y > 0.3 + 0.6 * uv.x"),
            wire("/drop", 0, "/liquid", FILL),
            wire("/slope", 0, "/liquid", OBSTACLES),
        ];
        ops.extend(outputs());
        let mut rig = Rig::new(gpu.clone(), Some(&dir), &ops);
        rig.pixels("/drop");
        rig.pixels("/slope");
        let (out, v, d) = run(&mut rig, 60);
        assert!(finite(&out) && finite(&v) && finite(&d));
        centre(&d)[0]
    };
    let (snow, water) = (slide("snow"), slide("liquid"));
    eprintln!("slope after 1 s: centre x of snow {snow:.3}, of liquid {water:.3}");
    // Both start with their centre at about 0.3 heights.
    assert!(snow < 0.4 && water > snow + 0.2, "{snow} {water}");
}

/// Emitter tables with sand, snow and unknown materials, a hostile fill of sand, absurd material
/// params, and sand and snow squeezed by three hundred times the earth's gravity: every output
/// stays finite.
#[test]
fn hostile_input_leaves_sand_and_snow_finite() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("liquid-hostile-granular");
    std::fs::write(dir.join("hostile.wgsl"), HOSTILE).unwrap();
    let rows = "x,y,width,vx,vy,on,material\n0.3,0.1,0.1,0,1,1,sand\n0.6,0.1,0.1,0,1,1,2\n\
                0.5,0.5,1e9,-1e9,1e9,1,snow\n0.5,0.1,0.1,0,1,1,lava\n0.5,0.1,0.1,0,1,1,7\n";
    let mut ops = vec![
        liquid(&[("grid", Value::Int(32)), ("fill_material", "sand".into())]),
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
    ops.extend(drawn());
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    rig.pixels("/bad");
    let all = run_drawn(&mut rig, 30);
    assert!(all.iter().all(finite));
    assert!(particles(&all[2]) > 100.0, "{}", particles(&all[2]));
    rig.edit(&[
        set("/liquid.gravity", Value::Vec2([0.0, 1000.0])),
        set("/liquid.sand_angle", Value::Float(1e9)),
        set("/liquid.snow_wetness", Value::Float(-1e9)),
    ]);
    let all = run_drawn(&mut rig, 30);
    assert!(all.iter().all(finite));
    rig.edit(&[set("/liquid.gravity", Value::Vec2([0.0, 3.27]))]);
    let all = run_drawn(&mut rig, 60);
    assert!(all.iter().all(finite));
    assert!(particles(&all[2]) > 100.0, "{}", particles(&all[2]));
}

/// Times the node on the hardware GPU: at the defaults (1920 x 1080, grid 384, 65536 particles,
/// most of them in a pool and a pour), and at the size of the compute fixture's timing (65536
/// particles, 384 cells per picture height, 37 substeps). The defaults again with a still
/// obstacle (flooded once) and with a moving one (flooded every frame), and with every output
/// read; the defaults again with sand and with snow in place of the liquid. Prints the wall time
/// per frame waited for, and per frame of frames cooked back to back.
#[test]
#[ignore = "times the liquid on the hardware GPU: cargo test -p penumbra-ops --release liquid_timing -- --ignored --nocapture"]
fn liquid_timing() {
    let gpu = penumbra_gpu::Gpu::headless(penumbra_gpu::AdapterChoice::Hardware).unwrap();
    eprintln!("{}", gpu.name());
    let dir = scratch("liquid-timing");
    let still = "abs(uv.x - 0.5) < 0.2 && abs(uv.y - 0.5) < 0.03";
    let shelf = "abs(uv.x - 0.5 - 0.2 * sin(t)) < 0.2 && abs(uv.y - 0.5) < 0.03";
    for (name, width, height, grid, particles, inside, obstacle, every, material) in [
        ("small", 1920, 1080, 256, 16384, "uv.y > 0.9", "", false, "liquid"),
        ("defaults", 1920, 1080, 384, 65536, "uv.y > 0.82", "", false, "liquid"),
        ("defaults, every output", 1920, 1080, 384, 65536, "uv.y > 0.82", "", true, "liquid"),
        ("defaults, still obstacle", 1920, 1080, 384, 65536, "uv.y > 0.82", still, false, "liquid"),
        ("defaults, moving obstacle", 1920, 1080, 384, 65536, "uv.y > 0.82", shelf, false, "liquid"),
        ("large", 1920, 1080, 512, 131_072, "uv.y > 0.8", "", false, "liquid"),
        ("fixture size", 1440, 1080, 512, 65536, "uv.x < 0.25 && uv.y > 2.0 / 3.0", "", false, "liquid"),
        ("defaults, sand", 1920, 1080, 384, 65536, "uv.y > 0.82", "", false, "sand"),
        ("defaults, snow", 1920, 1080, 384, 65536, "uv.y > 0.82", "", false, "snow"),
    ] {
        std::fs::write(
            dir.join("fill.wgsl"),
            format!(
                "@fragment\nfn main(v: PnVertex) -> @location(0) vec4f {{\n    let uv = v.uv;\n    return vec4f(vec3f(select(0.0, 1.0, {inside})), 1.0);\n}}\n"
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("obstacle.wgsl"),
            format!(
                "@fragment\nfn main(v: PnVertex) -> @location(0) vec4f {{\n    let uv = v.uv;\n    {}\n    \
                 return vec4f(vec3f(select(0.0, 1.0, {})), 1.0);\n}}\n",
                // Only a mask that reads the clock renders anew every frame.
                if obstacle.contains("(t)") { "let t = pn_time.seconds;" } else { "" },
                if obstacle.is_empty() { "false" } else { obstacle }
            ),
        )
        .unwrap();
        let mut ops = vec![
            create(
                "/liquid",
                "tex.liquid",
                &[
                    ("width", Value::Int(width)),
                    ("height", Value::Int(height)),
                    ("grid", Value::Int(grid)),
                    ("particles", Value::Int(particles)),
                    ("fill_material", material.into()),
                ],
            ),
            create(
                "/fill",
                "tex.shader",
                &[("file", "fill.wgsl".into()), ("width", Value::Int(width)), ("height", Value::Int(height))],
            ),
            wire("/fill", 0, "/liquid", FILL),
            create("/tap", "tex.liquid_emitter", &[("material", material.into())]),
            wire("/tap", 0, "/liquid", 0),
        ];
        if !obstacle.is_empty() {
            ops.push(create(
                "/obstacle",
                "tex.shader",
                &[
                    ("file", "obstacle.wgsl".into()),
                    ("width", Value::Int(width)),
                    ("height", Value::Int(height)),
                ],
            ));
            ops.push(wire("/obstacle", 0, "/liquid", OBSTACLES));
        }
        if every {
            ops.extend(outputs());
            ops.extend(drawn());
        }
        let mut rig = Rig::new(gpu.clone(), Some(&dir), &ops);
        rig.pixels("/fill");
        if !obstacle.is_empty() {
            rig.pixels("/obstacle");
        }
        let id = rig.id("/liquid");
        let ids: Vec<_> = if every {
            ["/liquid", "/v", "/d", "/f", "/g", "/s"].iter().map(|p| rig.id(p)).collect()
        } else {
            vec![id]
        };
        let frames = 300;
        let mut wall = Vec::new();
        for f in 0..frames {
            rig.clock.advance(1);
            rig.gpu.poll();
            let t0 = std::time::Instant::now();
            rig.cooker.cook(&rig.net, rig.clock.now(), &ids, &[]);
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
            rig.cooker.cook(&rig.net, rig.clock.now(), &ids, &[]);
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
