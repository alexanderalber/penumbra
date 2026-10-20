//! `tex.fluid` on WARP: dye moves the way the force pushes, projection removes divergence,
//! fading, reset, a still show, hostile input, a restored state.

use penumbra_core::Value;
use penumbra_gpu::Pixels;
use penumbra_graph::Op;

use super::{Rig, create, gpu, scratch, set};

/// A small fluid: a 64 x 32 picture on a 32 x 16 grid.
fn fluid(params: &[(&str, Value)]) -> Op {
    let mut all: Vec<(&str, Value)> = vec![
        ("width", Value::Int(64)),
        ("height", Value::Int(32)),
        ("size", Value::Int(32)),
        ("vorticity", Value::Float(0.0)),
    ];
    all.extend(params.iter().cloned());
    create("/fluid", "tex.fluid", &all)
}

/// A splat table node with these rows under the header `x,y,vx,vy`.
fn table(rows: &str) -> Op {
    create(
        "/splats",
        "table.text",
        &[("text", format!("x,y,vx,vy\n{rows}").into()), ("format", "csv".into())],
    )
}

fn wire(from: &str, output: u32, to: &str, input: u32) -> Op {
    Op::Connect { from: from.parse().unwrap(), output, to: to.parse().unwrap(), input }
}

/// Moves the show on a frame and reads `path`'s first output.
fn frame(rig: &mut Rig, path: &str) -> Pixels {
    rig.clock.advance(1);
    rig.cook(path);
    let tex = rig.tex(path).unwrap();
    rig.gpu.read(&tex).unwrap()
}

/// Sum of the red channel and its centre of mass (x, y in 0..1).
fn mass(px: &Pixels) -> (f64, [f64; 2]) {
    let (mut sum, mut mx, mut my) = (0.0, 0.0, 0.0);
    for y in 0..px.height {
        for x in 0..px.width {
            let r = f64::from(px.get(x, y).unwrap()[0]);
            sum += r;
            mx += r * (f64::from(x) + 0.5) / f64::from(px.width);
            my += r * (f64::from(y) + 0.5) / f64::from(px.height);
        }
    }
    (sum, if sum > 0.0 { [mx / sum, my / sum] } else { [0.0; 2] })
}

fn all_finite(px: &Pixels) -> bool {
    px.data.iter().flatten().all(|v| v.is_finite())
}

#[test]
fn dye_moves_the_way_the_force_pushes() {
    let Some(gpu) = gpu() else { return };
    for push in [0.004, -0.004] {
        let ops = [
            fluid(&[
                ("inject", "everywhere".into()),
                ("color", Value::Vec4([1.0, 0.0, 0.0, 1.0])),
                ("edges", "open".into()),
            ]),
            create(
                "/push",
                "tex.constant",
                &[
                    ("width", Value::Int(8)),
                    ("height", Value::Int(8)),
                    ("color", Value::Vec4([push, 0.0, 0.0, 1.0])),
                ],
            ),
            table("0.5,0.5,0,0"),
            wire("/push", 0, "/fluid", 0),
            wire("/splats", 0, "/fluid", 3),
        ];
        let mut rig = Rig::new(gpu.clone(), None, &ops);
        let mut px = frame(&mut rig, "/fluid");
        for _ in 0..30 {
            px = frame(&mut rig, "/fluid");
        }
        assert!(all_finite(&px));
        let (sum, [x, y]) = mass(&px);
        assert!(sum > 1.0, "{sum}");
        assert!((y - 0.5).abs() < 0.03, "{y}");
        if push > 0.0 {
            assert!(x > 0.56, "pushed right, centre at {x}");
        } else {
            assert!(x < 0.44, "pushed left, centre at {x}");
        }
    }
}

/// Mean absolute divergence of a velocity picture (image sizes per second) in its interior,
/// relative to the mean speed, both per cell.
fn divergence(px: &Pixels) -> f64 {
    let aspect = f64::from(px.width) / f64::from(px.height);
    let v = |x: u32, y: u32| {
        let c = px.get(x, y).unwrap();
        [f64::from(c[0]) * aspect, f64::from(c[1])]
    };
    let (mut div, mut speed, mut n) = (0.0, 0.0, 0.0);
    for y in 2..px.height - 2 {
        for x in 2..px.width - 2 {
            let d = (v(x + 1, y)[0] - v(x - 1, y)[0] + v(x, y + 1)[1] - v(x, y - 1)[1]) * 0.5;
            div += d.abs();
            let c = v(x, y);
            speed += (c[0] * c[0] + c[1] * c[1]).sqrt();
            n += 1.0;
        }
    }
    div / n / (speed / n).max(1e-9)
}

#[test]
fn projection_takes_the_divergence_out_of_a_push() {
    let Some(gpu) = gpu() else { return };
    let mut seen = Vec::new();
    for iterations in [1, 60] {
        let ops = [
            fluid(&[
                ("iterations", Value::Int(iterations)),
                ("velocity_fade", Value::Float(100.0)),
                ("splat_radius", Value::Float(0.2)),
            ]),
            table("0.4,0.5,1.5,0"),
            create("/v", "tex.null", &[]),
            wire("/splats", 0, "/fluid", 3),
            wire("/fluid", 1, "/v", 0),
        ];
        let mut rig = Rig::new(gpu.clone(), None, &ops);
        let mut px = frame(&mut rig, "/v");
        for _ in 0..10 {
            px = frame(&mut rig, "/v");
        }
        assert!(all_finite(&px));
        let right = px.get(px.width * 4 / 10, px.height / 2).unwrap()[0];
        assert!(right > 0.01, "the push moves right: {right}");
        seen.push(divergence(&px));
    }
    // A push is a source on one side and a sink on the other; the pressure turns it into a pair
    // of swirls.
    // (A grid that keeps velocity and pressure at the same points cannot get to zero: the
    // remainder is mostly in the finest ripples.)
    assert!(seen[1] < 0.03, "{seen:?}");
    assert!(seen[1] < seen[0] * 0.2, "{seen:?}");
}

#[test]
fn dye_fades_by_its_half_life_and_reset_clears() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        fluid(&[
            ("inject", "everywhere".into()),
            ("color", Value::Vec4([1.0, 0.0, 0.0, 1.0])),
            ("dye_fade", Value::Float(0.5)),
        ]),
        table("0.5,0.5,0,0"),
        wire("/splats", 0, "/fluid", 3),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    for _ in 0..10 {
        frame(&mut rig, "/fluid");
    }
    rig.edit(&[set("/splats.text", "x,y".into())]);
    let (before, _) = mass(&frame(&mut rig, "/fluid"));
    let mut px = frame(&mut rig, "/fluid");
    for _ in 0..29 {
        px = frame(&mut rig, "/fluid");
    }
    // Half a second later: half as much.
    let (after, _) = mass(&px);
    assert!(before > 1.0, "{before}");
    let ratio = after / before;
    assert!((ratio - 0.5).abs() < 0.08, "{after} of {before}: {ratio}");

    rig.edit(&[set("/fluid.reset", Value::Bool(true))]);
    let (cleared, _) = mass(&frame(&mut rig, "/fluid"));
    assert_eq!(cleared, 0.0);
    rig.edit(&[set("/fluid.reset", Value::Bool(false)), set("/splats.text", "x,y\n0.5,0.5".into())]);
    frame(&mut rig, "/fluid");
    let (again, _) = mass(&frame(&mut rig, "/fluid"));
    assert!(again > 0.0);
    // node.reset starts the node over too.
    rig.edit(&[set("/splats.text", "x,y".into())]);
    let id = rig.id("/fluid");
    rig.cooker.reset(id);
    let (fresh, _) = mass(&frame(&mut rig, "/fluid"));
    assert_eq!(fresh, 0.0);
}

#[test]
fn a_still_show_holds_the_fluid() {
    let Some(gpu) = gpu() else { return };
    let ops =
        [fluid(&[("inject", "everywhere".into())]), table("0.5,0.5,1,0"), wire("/splats", 0, "/fluid", 3)];
    let mut rig = Rig::new(gpu, None, &ops);
    for _ in 0..5 {
        frame(&mut rig, "/fluid");
    }
    let version = rig.version("/fluid");
    // A new input while the time stands still: nothing moves, nothing is added.
    rig.edit(&[set("/splats.text", "x,y,vx,vy\n0.2,0.2,-1,0".into())]);
    rig.cook("/fluid");
    assert_eq!(rig.version("/fluid"), version);
    rig.clock.advance(1);
    rig.cook("/fluid");
    assert_ne!(rig.version("/fluid"), version);
}

const HOSTILE: &str = "\
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let zero = v.uv.x - v.uv.x;
    let p = vec2i(v.pos.xy);
    if ((p.x + p.y) % 3 == 0) {
        return vec4f(zero / zero, 1.0 / zero, -1.0 / zero, zero / zero);
    }
    return vec4f(0.01, 0.0, 0.5, 1.0);
}
";

#[test]
fn nan_and_infinity_in_the_inputs_do_not_poison_the_fluid() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("fluid-hostile");
    std::fs::write(dir.join("hostile.wgsl"), HOSTILE).unwrap();
    let ops = [
        fluid(&[("inject", "everywhere".into()), ("amount", Value::Float(0.5))]),
        create(
            "/bad",
            "tex.shader",
            &[("file", "hostile.wgsl".into()), ("width", Value::Int(64)), ("height", Value::Int(32))],
        ),
        create("/v", "tex.null", &[]),
        wire("/bad", 0, "/fluid", 0),
        wire("/bad", 0, "/fluid", 1),
        wire("/bad", 0, "/fluid", 2),
        wire("/fluid", 1, "/v", 0),
    ];
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    let mut dye = frame(&mut rig, "/fluid");
    for _ in 0..20 {
        dye = frame(&mut rig, "/fluid");
    }
    let velocity = frame(&mut rig, "/v");
    assert!(all_finite(&dye) && all_finite(&velocity));
    // The clean pixels still pour dye in.
    assert!(mass(&dye).0 > 0.0);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_restored_fluid_carries_on_at_another_size() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        fluid(&[
            ("inject", "everywhere".into()),
            ("color", Value::Vec4([1.0, 0.0, 0.0, 1.0])),
            ("dye_fade", Value::Float(100.0)),
        ]),
        table("0.25,0.5,0,0"),
        wire("/splats", 0, "/fluid", 3),
    ];
    let mut rig = Rig::new(gpu.clone(), None, &ops);
    for _ in 0..10 {
        frame(&mut rig, "/fluid");
    }
    let (saved, [x, _]) = mass(&frame(&mut rig, "/fluid"));
    let kept = rig.cooker.kept();
    let [(_, penumbra_graph::Kept::State(items))] = kept.as_slice() else { panic!("{kept:?}") };
    assert_eq!(items.len(), 2);

    // A restart at twice the size, without the splat: the dye is there, where it was.
    let mut again = Rig::new(gpu, None, &[ops[0].clone()]);
    again.edit(&[set("/fluid.width", Value::Int(128)), set("/fluid.height", Value::Int(64))]);
    again.cooker.cook(&again.net, again.clock.now(), &[], &[]);
    again.cooker.restore(again.id("/fluid"), penumbra_graph::Kept::State(items.clone())).unwrap();
    let px = frame(&mut again, "/fluid");
    assert_eq!((px.width, px.height), (128, 64));
    let (sum, [rx, _]) = mass(&px);
    // Four times the pixels.
    assert!((sum / (4.0 * saved) - 1.0).abs() < 0.1, "{sum} after {saved}");
    assert!((rx - x).abs() < 0.03, "{rx} after {x}");
}

#[test]
fn poured_dye_takes_on_its_colour_and_no_more() {
    let Some(gpu) = gpu() else { return };
    let small = |path: &str, rgba: [f64; 4]| {
        create(
            path,
            "tex.constant",
            &[("width", Value::Int(8)), ("height", Value::Int(8)), ("color", Value::Vec4(rgba))],
        )
    };
    let ops = [
        fluid(&[("inject", "everywhere".into())]),
        small("/paint", [0.5, 0.25, 0.0, 1.0]),
        wire("/paint", 0, "/fluid", 1),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    let mut px = frame(&mut rig, "/fluid");
    for _ in 0..60 {
        px = frame(&mut rig, "/fluid");
    }
    let s = px.stats();
    assert!(s.max[0] <= 0.501 && s.min[0] > 0.45, "{s:?}");
    assert!((s.mean[1] - 0.25).abs() < 0.02 && s.max[3] <= 1.0, "{s:?}");
}

#[test]
fn the_fluid_flows_around_obstacles() {
    let Some(gpu) = gpu() else { return };
    // A wall across the middle, from top to bottom; the push and the dye start left of it.
    const WALL: &str = "@fragment fn main(v: PnVertex) -> @location(0) vec4f {         return vec4f(vec3f(select(0.0, 1.0, abs(v.uv.x - 0.5) < 0.1)), 1.0); }";
    let dir = scratch("fluid-wall");
    std::fs::write(dir.join("wall.wgsl"), WALL).unwrap();
    let ops = [
        fluid(&[
            ("inject", "everywhere".into()),
            ("color", Value::Vec4([1.0, 0.0, 0.0, 1.0])),
            ("splat_radius", Value::Float(0.15)),
            ("edges", "open".into()),
        ]),
        create(
            "/wall",
            "tex.shader",
            &[("file", "wall.wgsl".into()), ("width", Value::Int(64)), ("height", Value::Int(32))],
        ),
        table("0.25,0.5,2,0"),
        wire("/wall", 0, "/fluid", 2),
        wire("/splats", 0, "/fluid", 3),
    ];
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    let mut px = frame(&mut rig, "/fluid");
    for _ in 0..60 {
        px = frame(&mut rig, "/fluid");
    }
    let (sum, _) = mass(&px);
    let beyond: f64 = (0..px.height)
        .flat_map(|y| (px.width * 5 / 8..px.width).map(move |x| (x, y)))
        .map(|(x, y)| f64::from(px.get(x, y).unwrap()[0]))
        .sum();
    assert!(sum > 10.0, "{sum}");
    assert!(beyond < sum * 0.01, "{beyond} of {sum} got through the wall");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn viscosity_spreads_a_push_out() {
    let Some(gpu) = gpu() else { return };
    let mut peaks = Vec::new();
    for viscosity in [0.0, 0.01] {
        let ops = [
            fluid(&[("viscosity", Value::Float(viscosity))]),
            table("0.5,0.5,1,0"),
            create("/v", "tex.null", &[]),
            wire("/splats", 0, "/fluid", 3),
            wire("/fluid", 1, "/v", 0),
        ];
        let mut rig = Rig::new(gpu.clone(), None, &ops);
        let mut px = frame(&mut rig, "/v");
        for _ in 0..10 {
            px = frame(&mut rig, "/v");
        }
        assert!(all_finite(&px));
        peaks.push(px.stats().max[0]);
    }
    assert!(peaks[1] > 0.0 && peaks[1] < peaks[0] * 0.8, "{peaks:?}");
}
