//! `vol.smoke` drawn by `tex.raymarch` on WARP: hot smoke rises against gravity, a jet blows
//! along its velocity, projection keeps the volume's flow free of divergence, reset, a still
//! show, a restored state, the density output, burning, vorticity, the box emitter, the glow and
//! emitters from tables.

use penumbra_core::Value;
use penumbra_gpu::Pixels;
use penumbra_graph::Op;

use super::{Rig, connect, create, gpu, set};

/// The params of `vol.emitter`, as the rig below names them, and their names on the node.
const EMITTER_PARAMS: [(&str, &str); 9] = [
    ("emitter_position", "position"),
    ("emitter_type", "type"),
    ("emitter_size", "size"),
    ("emitter_radius", "radius"),
    ("linear_velocity", "linear_velocity"),
    ("smoke", "smoke"),
    ("temp", "temp"),
    ("fuel", "fuel"),
    ("correction_rate", "correction_rate"),
];

/// A small box, 2 x 1 x 1 metres on a 24 x 12 x 12 grid, drawn 48 x 24. One `vol.emitter`,
/// fed by the params `EMITTER_PARAMS` names; the rest go to `vol.smoke`.
fn smoke_rig(params: &[(&str, Value)]) -> Rig {
    let mut all: Vec<(&str, Value)> = vec![
        ("size", Value::Vec3([2.0, 1.0, 1.0])),
        ("resolution", Value::Int(24)),
        ("emitter_position", Value::Vec3([0.0, -0.3, 0.0])),
        ("emitter_radius", Value::Float(0.15)),
        ("linear_velocity", Value::Vec3([0.0; 3])),
        ("smoke", Value::Float(1.0)),
        ("iterations", Value::Int(10)),
        // Half of the way each 60th of a second, so the few frames of a test fill the emitter.
        ("correction_rate", Value::Float(30.0)),
        ("fuel", Value::Float(0.0)),
    ];
    all.extend(params.iter().cloned());
    let mut smoke: Vec<(&str, Value)> = Vec::new();
    let mut emit: Vec<(&str, Value)> = Vec::new();
    for (name, value) in all {
        match EMITTER_PARAMS.iter().find(|(n, _)| *n == name) {
            Some((_, on_node)) => emit.push((on_node, value)),
            None => smoke.push((name, value)),
        }
    }
    let ops = [
        create("/emit", "vol.emitter", &emit),
        create("/smoke", "vol.smoke", &smoke),
        create("/march", "tex.raymarch", &[("width", Value::Int(48)), ("height", Value::Int(24))]),
        connect("/emit", "/smoke", 0),
        connect("/smoke", "/march", 0),
    ];
    Rig::new(gpu().unwrap(), None, &ops)
}

/// The same box and picture with its emitters from tables (one input per table).
fn table_rig(tables: &[String], params: &[(&str, Value)]) -> Rig {
    let mut smoke: Vec<(&str, Value)> = vec![
        ("size", Value::Vec3([2.0, 1.0, 1.0])),
        ("resolution", Value::Int(24)),
        ("iterations", Value::Int(10)),
        ("buoyancy", Value::Float(0.0)),
    ];
    smoke.extend(params.iter().cloned());
    let mut ops = vec![
        create("/smoke", "vol.smoke", &smoke),
        create("/march", "tex.raymarch", &[("width", Value::Int(48)), ("height", Value::Int(24))]),
        connect("/smoke", "/march", 0),
    ];
    for (i, text) in tables.iter().enumerate() {
        let path = format!("/t{i}");
        ops.push(create(&path, "table.text", &[("text", text.as_str().into()), ("format", "csv".into())]));
        ops.push(connect(&path, "/smoke", i as u32));
    }
    Rig::new(gpu().unwrap(), None, &ops)
}

fn frame(rig: &mut Rig) -> Pixels {
    rig.clock.advance(1);
    rig.cook("/march");
    let tex = rig.tex("/march").unwrap();
    rig.gpu.read(&tex).unwrap()
}

/// Sum of the alpha channel and its centre (x, y in 0..1 from the top left).
fn cover(px: &Pixels) -> (f64, [f64; 2]) {
    let (mut sum, mut mx, mut my) = (0.0, 0.0, 0.0);
    for y in 0..px.height {
        for x in 0..px.width {
            let a = f64::from(px.get(x, y).unwrap()[3]);
            sum += a;
            mx += a * (f64::from(x) + 0.5) / f64::from(px.width);
            my += a * (f64::from(y) + 0.5) / f64::from(px.height);
        }
    }
    (sum, if sum > 0.0 { [mx / sum, my / sum] } else { [0.0; 2] })
}

fn finite(px: &Pixels) -> bool {
    px.data.iter().flatten().all(|v| v.is_finite())
}

/// The first row from the top where the smoke covers more than a tenth.
fn top(px: &Pixels) -> u32 {
    (0..px.height).find(|y| (0..px.width).any(|x| px.get(x, *y).unwrap()[3] > 0.1)).unwrap_or(px.height)
}

#[test]
fn hot_smoke_rises_against_gravity() {
    if gpu().is_none() {
        return;
    }
    // The same slow upward puff, with and without buoyancy, and with gravity turned round.
    let run = |params: &[(&str, Value)]| {
        let mut all = vec![
            ("linear_velocity", Value::Vec3([0.0, 0.3, 0.0])),
            ("vorticity_strength", Value::Float(0.0)),
            // Smoothed, the puff's top edge spreads, so the rows between the runs are clearer.
            ("advection", Value::Text("smooth".into())),
        ];
        all.extend(params.iter().cloned());
        let mut rig = smoke_rig(&all);
        let mut px = frame(&mut rig);
        for _ in 0..30 {
            px = frame(&mut rig);
        }
        assert!(finite(&px));
        let (_, [x, _]) = cover(&px);
        assert!((x - 0.5).abs() < 0.05, "{x}");
        px
    };
    let calm = run(&[("buoyancy", Value::Float(0.0)), ("linear_velocity", Value::Vec3([0.0; 3]))]);
    let still = top(&run(&[("buoyancy", Value::Float(0.0))]));
    // Temperatures stop at 1, so a stronger buoyancy than the default makes the rise plain.
    let lifted = top(&run(&[("buoyancy", Value::Float(10.0))]));
    assert!(lifted + 2 < still, "top at row {lifted} with buoyancy, {still} without");
    let sunk = run(&[
        ("gravity", Value::Vec3([0.0, 1.0, 0.0])),
        ("buoyancy", Value::Float(10.0)),
        ("linear_velocity", Value::Vec3([0.0; 3])),
    ]);
    // Heavier than the calm puff's, it sinks below it.
    let (_, [_, y]) = cover(&sunk);
    let (_, [_, calm]) = cover(&calm);
    assert!(y > calm + 0.01, "{y} against {calm}");
}

#[test]
fn a_jet_blows_along_its_velocity() {
    if gpu().is_none() {
        return;
    }
    let mut rig = smoke_rig(&[
        ("emitter_position", Value::Vec3([-0.6, 0.0, 0.0])),
        ("linear_velocity", Value::Vec3([3.0, 0.0, 0.0])),
        ("buoyancy", Value::Float(0.0)),
    ]);
    let mut px = frame(&mut rig);
    for _ in 0..30 {
        px = frame(&mut rig);
    }
    let (_, [x, y]) = cover(&px);
    // The emitter is at 0.2 of the width; the jet carries the smoke right, its front past the
    // middle.
    assert!(x > 0.3, "{x}");
    assert!((y - 0.5).abs() < 0.05, "{y}");
    let front = (0..px.width).rev().find(|x| px.get(*x, 12).unwrap()[3] > 0.1).unwrap();
    assert!(front > px.width / 2, "front at {front} of {}", px.width);
    // Turned half way round about y, the jet points left: the picture mirrored.
    rig.edit(&[set("/march.rotate", Value::Vec3([0.0, 180.0, 0.0])), set("/smoke.speed", Value::Float(0.0))]);
    let (_, [turned, _]) = cover(&frame(&mut rig));
    assert!((turned - (1.0 - x)).abs() < 0.02, "{turned} against {x}");
}

#[test]
fn reset_clears_and_a_still_show_holds_the_smoke() {
    if gpu().is_none() {
        return;
    }
    let mut rig = smoke_rig(&[]);
    for _ in 0..10 {
        frame(&mut rig);
    }
    let version = rig.version("/smoke");
    // The show stands still: nothing new.
    rig.cook("/march");
    assert_eq!(rig.version("/smoke"), version);
    rig.edit(&[set("/smoke.reset", true.into()), set("/emit.smoke", Value::Float(0.0))]);
    let (sum, _) = cover(&frame(&mut rig));
    assert!(sum < 1e-3, "{sum}");
}

#[test]
fn hostile_params_keep_the_smoke_finite() {
    if gpu().is_none() {
        return;
    }
    let mut rig = smoke_rig(&[
        ("linear_velocity", Value::Vec3([1000.0, -1000.0, 1000.0])),
        ("buoyancy", Value::Float(1000.0)),
        ("temp", Value::Float(1000.0)),
        ("smoke", Value::Float(1000.0)),
        ("gravity", Value::Vec3([0.0, 0.0, 0.0])),
    ]);
    for _ in 0..10 {
        let px = frame(&mut rig);
        assert!(finite(&px));
    }
    let s = rig.cooker.status(rig.id("/smoke")).unwrap();
    assert!(s.error.is_none(), "{s:?}");
}

#[test]
fn a_restored_smoke_carries_on_where_it_was() {
    if gpu().is_none() {
        return;
    }
    let mut rig = smoke_rig(&[("smoke_fade", Value::Float(0.0)), ("smoke_damping", Value::Float(0.0))]);
    for _ in 0..20 {
        frame(&mut rig);
    }
    let (saved, [_, y]) = cover(&frame(&mut rig));
    let kept = rig.cooker.kept();
    let items = kept
        .iter()
        .find_map(|(id, k)| match k {
            penumbra_graph::Kept::State(items) if *id == rig.id("/smoke") => Some(items.clone()),
            _ => None,
        })
        .expect("the smoke keeps its state");
    assert_eq!(items.len(), 3);

    // A restart with the emitter off: the smoke is there, near where it was.
    let mut again = smoke_rig(&[("smoke_fade", Value::Float(0.0)), ("emitter_radius", Value::Float(0.0))]);
    again.cooker.cook(&again.net, again.clock.now(), &[], &[]);
    again.cooker.restore(again.id("/smoke"), penumbra_graph::Kept::State(items)).unwrap();
    let (sum, [_, ry]) = cover(&frame(&mut again));
    assert!(sum > 0.5 * saved, "{sum} after {saved}");
    assert!((ry - y).abs() < 0.1, "{ry} after {y}");
}

#[test]
fn the_density_output_is_the_cover_as_opaque_grey() {
    if gpu().is_none() {
        return;
    }
    let mut rig = smoke_rig(&[]);
    rig.edit(&[
        create("/d", "tex.null", &[]),
        Op::Connect { from: "/march".parse().unwrap(), output: 1, to: "/d".parse().unwrap(), input: 0 },
    ]);
    for _ in 0..10 {
        frame(&mut rig);
    }
    rig.clock.advance(1);
    rig.cook("/d");
    let out = rig.gpu.read(&rig.tex("/march").unwrap()).unwrap();
    let d = rig.gpu.read(&rig.tex("/d").unwrap()).unwrap();
    let mut most = 0.0f32;
    for y in 0..d.height {
        for x in 0..d.width {
            let [r, g, b, a] = d.get(x, y).unwrap();
            assert_eq!(a, 1.0);
            assert!(r == g && g == b);
            assert!((r - out.get(x, y).unwrap()[3]).abs() < 1e-3);
            most = most.max(r);
        }
    }
    assert!(most > 0.1, "{most}");
}

/// A jet blowing right for `frames` frames; the picture and the smoke's status.
fn jet(params: &[(&str, Value)], march: &[(&str, Value)], frames: usize) -> Pixels {
    let mut all = vec![
        ("emitter_position", Value::Vec3([-0.6, 0.0, 0.0])),
        ("linear_velocity", Value::Vec3([3.0, 0.0, 0.0])),
        ("buoyancy", Value::Float(0.0)),
    ];
    all.extend(params.iter().cloned());
    let mut rig = smoke_rig(&all);
    let edits: Vec<Op> = march.iter().map(|(k, v)| set(&format!("/march.{k}"), v.clone())).collect();
    rig.edit(&edits);
    let mut px = frame(&mut rig);
    for _ in 0..frames {
        px = frame(&mut rig);
        assert!(finite(&px));
    }
    let s = rig.cooker.status(rig.id("/smoke")).unwrap();
    assert!(s.error.is_none(), "{s:?}");
    px
}

/// How many rows the smoke covers by more than a tenth, in the column at `x` (0..1).
fn height_at(px: &Pixels, x: f64) -> u32 {
    let col = ((x * f64::from(px.width)) as u32).min(px.width - 1);
    (0..px.height).filter(|y| px.get(col, *y).unwrap()[3] > 0.1).count() as u32
}

#[test]
fn fuel_burns_into_smoke_that_swells() {
    if gpu().is_none() {
        return;
    }
    let thin = [("thickness", Value::Float(1.0))];
    let cold = jet(&[("smoke", Value::Float(0.3))], &thin, 30);
    let burning = jet(&[("smoke", Value::Float(0.3)), ("fuel", Value::Float(2.0))], &thin, 30);
    let (sum_cold, _) = cover(&cold);
    let (sum_burning, _) = cover(&burning);
    // Burning makes smoke, and the gas swells: more cover, a wider plume downstream.
    assert!(sum_burning > sum_cold * 1.2, "{sum_burning} against {sum_cold}");
    assert!(
        height_at(&burning, 0.6) > height_at(&cold, 0.6),
        "{} against {}",
        height_at(&burning, 0.6),
        height_at(&cold, 0.6)
    );
}

#[test]
fn vorticity_a_box_emitter_and_fire_stay_finite() {
    if gpu().is_none() {
        return;
    }
    let px = jet(
        &[
            ("emitter_type", Value::Text("box".into())),
            ("emitter_size", Value::Vec3([0.3, 0.3, 0.3])),
            ("fuel", Value::Float(4.0)),
            ("vorticity_strength", Value::Float(40.0)),
            ("expansion", Value::Float(10.0)),
        ],
        &[],
        60,
    );
    let (sum, _) = cover(&px);
    assert!(sum > 1.0, "{sum}");
    // Sharp and smooth advection both carry the smoke.
    let smooth = jet(&[("advection", Value::Text("smooth".into()))], &[], 20);
    assert!(cover(&smooth).0 > 1.0);
}

#[test]
fn a_box_emitter_fills_its_box() {
    if gpu().is_none() {
        return;
    }
    let still = [("linear_velocity", Value::Vec3([0.0; 3])), ("buoyancy", Value::Float(0.0))];
    let mut ball = vec![("emitter_radius", Value::Float(0.2))];
    ball.extend(still.iter().cloned());
    let mut cube =
        vec![("emitter_type", Value::Text("box".into())), ("emitter_size", Value::Vec3([0.8, 0.2, 0.2]))];
    cube.extend(still.iter().cloned());
    let ball = jet(&ball, &[], 3);
    let cube = jet(&cube, &[], 3);
    // The box is wider than the ball and lower.
    let width = |px: &Pixels| (0..px.width).filter(|x| px.get(*x, 12).unwrap()[3] > 0.1).count();
    assert!(width(&cube) > width(&ball) + 3, "{} against {}", width(&cube), width(&ball));
    assert!(height_at(&cube, 0.2) < height_at(&ball, 0.2) + 1);
}

#[test]
fn hot_smoke_glows() {
    if gpu().is_none() {
        return;
    }
    let black = Value::Vec4([0.0, 0.0, 0.0, 1.0]);
    let dark = jet(&[("temp", Value::Float(2.0))], &[("color", black.clone())], 10);
    let glowing = jet(&[("temp", Value::Float(2.0))], &[("color", black), ("glow", Value::Float(1.0))], 10);
    let light = |px: &Pixels| px.data.iter().map(|p| f64::from(p[0] + p[1] + p[2])).sum::<f64>();
    assert!(light(&dark) < 1e-3, "{}", light(&dark));
    assert!(light(&glowing) > 1.0, "{}", light(&glowing));
}

#[test]
fn burning_smoke_out_through_an_open_edge_keeps_going() {
    if gpu().is_none() {
        return;
    }
    // Hot burning smoke swirling out of the box's far edge for ten seconds. It once spun up
    // there until the whole box blew empty, the emitter's own smoke included.
    let px = jet(
        &[
            ("emitter_type", Value::Text("box".into())),
            ("emitter_size", Value::Vec3([0.3, 0.3, 0.3])),
            ("fuel", Value::Float(4.0)),
            ("temp", Value::Float(2.65)),
            ("gravity", Value::Vec3([-0.7, 0.0, 0.0])),
            ("buoyancy", Value::Float(4.0)),
            ("vorticity_strength", Value::Float(10.0)),
        ],
        &[],
        600,
    );
    assert!(finite(&px));
    let (sum, _) = cover(&px);
    assert!(sum > 1.0, "{sum}");
}

/// Cover of the columns from `from` to `to` (0..1 of the width).
fn band(px: &Pixels, from: f64, to: f64) -> f64 {
    let (first, last) = ((from * f64::from(px.width)) as u32, (to * f64::from(px.width)) as u32);
    let mut sum = 0.0;
    for y in 0..px.height {
        for x in first..last.min(px.width) {
            sum += f64::from(px.get(x, y).unwrap()[3]);
        }
    }
    sum
}

const HEADER: &str = "x,y,z,radius,vx,vy,vz,smoke,fuel,correction_rate";

#[test]
fn two_emitters_put_smoke_in_two_places() {
    if gpu().is_none() {
        return;
    }
    let table = format!("{HEADER}\n-0.6,-0.2,0,0.15,0,0,0,1,0,60\n0.6,-0.2,0,0.15,0,0,0,1,0,60\n");
    let mut rig = table_rig(&[table], &[]);
    let mut px = frame(&mut rig);
    for _ in 0..10 {
        px = frame(&mut rig);
    }
    assert!(finite(&px));
    let (left, middle, right) = (band(&px, 0.0, 1.0 / 3.0), band(&px, 0.4, 0.6), band(&px, 2.0 / 3.0, 1.0));
    assert!(left > 1.0 && right > 1.0, "left {left}, right {right}");
    assert!(middle < left / 10.0, "middle {middle}, left {left}");
    let s = rig.cooker.status(rig.id("/smoke")).unwrap();
    assert!(s.error.is_none() && s.warning.is_none(), "{s:?}");
}

#[test]
fn emitters_on_two_inputs_add_up() {
    if gpu().is_none() {
        return;
    }
    let one = format!("{HEADER}\n-0.6,-0.2,0,0.15,0,0,0,1,0,60\n");
    let two = format!("{HEADER}\n0.6,-0.2,0,0.15,0,0,0,1,0,60\n");
    let mut rig = table_rig(&[one, two], &[]);
    let mut px = frame(&mut rig);
    for _ in 0..10 {
        px = frame(&mut rig);
    }
    let (left, right) = (band(&px, 0.0, 1.0 / 3.0), band(&px, 2.0 / 3.0, 1.0));
    assert!(left > 1.0 && right > 1.0, "left {left}, right {right}");
}

#[test]
fn the_later_of_two_overlapping_emitters_wins() {
    if gpu().is_none() {
        return;
    }
    let dense = "0,0,0,0.4,0,0,0,1,0,60";
    let clear = "0,0,0,0.4,0,0,0,0,0,60";
    let run = |rows: &str| {
        let mut rig = table_rig(&[format!("{HEADER}\n{rows}\n")], &[]);
        let mut px = frame(&mut rig);
        for _ in 0..4 {
            px = frame(&mut rig);
        }
        cover(&px).0
    };
    let cleared = run(&format!("{dense}\n{clear}"));
    let filled = run(&format!("{clear}\n{dense}"));
    // Inside, the later emitter sets the cells; only the soft rim of the first one shows.
    assert!(filled > 1.0, "{filled}");
    assert!(cleared < filled / 2.0, "{cleared} against {filled}");
}

#[test]
fn no_emitter_stays_empty_without_complaint() {
    if gpu().is_none() {
        return;
    }
    let mut rig = table_rig(&[], &[]);
    for _ in 0..5 {
        let px = frame(&mut rig);
        assert!(finite(&px));
        assert!(cover(&px).0 < 1e-3);
    }
    let s = rig.cooker.status(rig.id("/smoke")).unwrap();
    assert!(s.error.is_none() && s.warning.is_none(), "{s:?}");
}

#[test]
fn bad_rows_and_bad_values_are_left_out_of_the_smoke() {
    if gpu().is_none() {
        return;
    }
    let rows = [
        "-0.6,-0.2,0,0.15,0,0,0,1,0,60",
        "nan,0,0,0.15,0,0,0,1,0,60",
        "0.6,inf,0,0.15,0,0,0,1,0,60",
        "0.6,-0.2,0,not a number,0,0,0,1,0,60",
        "0.6,0,0",
        // Absurd but finite: far outside the box, and a huge size that may not blow anything up.
        "1e300,-1e300,0,1e300,1e300,1e300,1e300,1e300,1e300,1e300",
    ];
    let mut rig = table_rig(&[format!("{HEADER}\n{}\n", rows.join("\n"))], &[]);
    let mut px = frame(&mut rig);
    for _ in 0..10 {
        px = frame(&mut rig);
        assert!(finite(&px));
    }
    // Only the first row stays in the picture: smoke on the left, none on the right.
    let (left, right) = (band(&px, 0.0, 1.0 / 3.0), band(&px, 2.0 / 3.0, 1.0));
    assert!(left > 1.0, "{left}");
    assert!(right < left / 10.0, "right {right}, left {left}");
}

#[test]
fn a_table_without_x_and_y_warns_and_the_smoke_goes_on() {
    if gpu().is_none() {
        return;
    }
    let mut rig = table_rig(&["a,b\n1,2\n".to_string()], &[]);
    let px = frame(&mut rig);
    assert!(cover(&px).0 < 1e-3);
    let s = rig.cooker.status(rig.id("/smoke")).unwrap();
    assert!(s.error.is_none(), "{s:?}");
    let warning = s.warning.clone().unwrap_or_default();
    assert!(warning.contains("columns x and y"), "{warning}");
}

#[test]
fn more_than_the_limit_of_emitters_are_dropped_with_a_warning() {
    if gpu().is_none() {
        return;
    }
    let mut text = format!("{HEADER}\n");
    for k in 0..70 {
        text.push_str(&format!("{},0,0,0.02,0,0,0,1,0,60\n", -0.9 + f64::from(k) * 0.02));
    }
    let mut rig = table_rig(&[text], &[]);
    let px = frame(&mut rig);
    assert!(finite(&px));
    let s = rig.cooker.status(rig.id("/smoke")).unwrap();
    assert!(s.error.is_none(), "{s:?}");
    let warning = s.warning.clone().unwrap_or_default();
    assert!(warning.contains("more than 64 emitters"), "{warning}");
}

#[test]
fn a_table_row_and_a_vol_emitter_make_the_same_smoke() {
    if gpu().is_none() {
        return;
    }
    let mut from_node = smoke_rig(&[
        ("emitter_position", Value::Vec3([0.2, -0.2, 0.1])),
        ("linear_velocity", Value::Vec3([0.5, 0.25, 0.0])),
    ]);
    let mut from_table = table_rig(
        &["x,y,z,radius,vx,vy,vz,smoke,fuel,correction_rate\n0.2,-0.2,0.1,0.15,0.5,0.25,0,1,0,30\n"
            .to_string()],
        &[("buoyancy", Value::Float(4.0))],
    );
    let mut a = frame(&mut from_node);
    let mut b = frame(&mut from_table);
    for _ in 0..10 {
        a = frame(&mut from_node);
        b = frame(&mut from_table);
    }
    assert_eq!(a.data, b.data);
}
