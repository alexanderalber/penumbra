//! `iterations` on `tex.shader` on WARP: N passes equal N chained nodes, 1 is one pass, the
//! passes know their number, hostile counts are clamped, and the size rule with `custom`.

use penumbra_core::Value;
use penumbra_gpu::Pixels;
use penumbra_graph::Op;

use super::{Rig, connect, create, gpu, scratch};

/// A 16 x 8 pattern that differs in every pixel and channel.
const SEED: &str = "@fragment fn main(v: PnVertex) -> @location(0) vec4f { \
    let p = floor(v.pos.xy); \
    return vec4f(fract(p.x * 0.37), fract(p.y * 0.61), fract((p.x + p.y) * 0.13), 1.0); }";

/// One step of something simulation-like: neighbours, a second input and a param.
const STEP: &str = "// @param gain float = 0.5
@fragment fn main(v: PnVertex) -> @location(0) vec4f {
    let size = vec2i(textureDimensions(in0));
    let p = vec2i(floor(v.pos.xy));
    let c = textureLoad(in0, clamp(p, vec2i(0), size - 1), 0);
    let l = textureLoad(in0, clamp(p - vec2i(1, 0), vec2i(0), size - 1), 0);
    let r = textureLoad(in0, clamp(p + vec2i(1, 0), vec2i(0), size - 1), 0);
    let s = textureLoad(in1, clamp(p, vec2i(0), vec2i(textureDimensions(in1)) - 1), 0);
    return vec4f(c.rgb * params.gain + (l.rgb + r.rgb) * 0.2 + s.rgb * 0.1 + c.gbr * c.gbr * 0.1, 1.0);
}
";

/// Red counts the passes, green and blue are the pass's number and the count, alpha in0's width.
const COUNT: &str = "@fragment fn main(v: PnVertex) -> @location(0) vec4f { \
    let n = textureLoad(in0, vec2i(0), 0).r + 1.0; \
    return vec4f(n, f32(pn.iteration), f32(pn.iterations), f32(textureDimensions(in0).x)); }";

fn seed() -> Op {
    create(
        "/seed",
        "tex.shader",
        &[
            ("file", "seed.wgsl".into()),
            ("resolution", "custom".into()),
            ("width", Value::Int(16)),
            ("height", Value::Int(8)),
        ],
    )
}

fn step(path: &str, iterations: i64) -> Op {
    create(
        path,
        "tex.shader",
        &[("file", "step.wgsl".into()), ("gain", Value::Float(0.45)), ("iterations", Value::Int(iterations))],
    )
}

/// Runs `ops` in a scratch directory with the three shaders and reads `path`.
fn run(name: &str, ops: &[Op], path: &str) -> Option<Pixels> {
    let gpu = gpu()?;
    let dir = scratch(name);
    std::fs::write(dir.join("seed.wgsl"), SEED).unwrap();
    std::fs::write(dir.join("step.wgsl"), STEP).unwrap();
    std::fs::write(dir.join("count.wgsl"), COUNT).unwrap();
    let mut rig = Rig::new(gpu, Some(&dir), ops);
    Some(rig.pixels(path))
}

fn count(iterations: Value, size: Option<(i64, i64)>) -> Op {
    let mut params = vec![("file", "count.wgsl".into()), ("iterations", iterations)];
    if let Some((w, h)) = size {
        params.extend([("resolution", "custom".into()), ("width", Value::Int(w)), ("height", Value::Int(h))]);
    }
    create("/count", "tex.shader", &params)
}

#[test]
fn iterations_equal_a_chain_of_nodes() {
    const N: usize = 5;
    let mut chain = vec![seed()];
    for i in 0..N {
        chain.push(step(&format!("/s{i}"), 1));
        let from = if i == 0 { "/seed".to_string() } else { format!("/s{}", i - 1) };
        chain.push(connect(&from, &format!("/s{i}"), 0));
        chain.push(connect("/seed", &format!("/s{i}"), 1));
    }
    let Some(chained) = run("iter_chain", &chain, &format!("/s{}", N - 1)) else { return };
    let one = [seed(), step("/s", N as i64), connect("/seed", "/s", 0), connect("/seed", "/s", 1)];
    let Some(iterated) = run("iter_one", &one, "/s") else { return };
    assert_eq!((iterated.width, iterated.height), (16, 8));
    // The same passes on the same GPU: the same bits.
    assert_eq!(chained.data, iterated.data);
    // And the passes did something: one step alone differs.
    let single = [seed(), step("/s", 1), connect("/seed", "/s", 0), connect("/seed", "/s", 1)];
    let Some(once) = run("iter_once", &single, "/s") else { return };
    assert_ne!(once.data, iterated.data);
}

#[test]
fn one_iteration_is_one_pass_and_passes_know_their_number() {
    let Some(px) = run("iter_1", &[count(Value::Int(1), None)], "/count") else { return };
    // Unconnected in0 reads 0: one pass adds 1; pass 0 of 1; in0 is the 1 x 1 stand-in.
    assert_eq!(px.get(0, 0), Some([1.0, 0.0, 1.0, 1.0]));
    let Some(px) = run("iter_4", &[count(Value::Int(4), None)], "/count") else { return };
    assert_eq!(px.get(0, 0).map(|p| [p[0], p[1], p[2]]), Some([4.0, 3.0, 4.0]));
}

#[test]
fn hostile_iteration_counts_are_clamped() {
    for (name, n, want) in [("iter_zero", 0, 1.0), ("iter_negative", -7, 1.0), ("iter_huge", 1_000_000, 64.0)]
    {
        let Some(px) = run(name, &[count(Value::Int(n), None)], "/count") else { return };
        assert_eq!(px.get(0, 0).map(|p| [p[0], p[2]]), Some([want, want]), "{name}");
    }
}

#[test]
fn with_custom_size_later_passes_read_the_output_size() {
    // in0 is 16 x 8, the output 4 x 2.
    let ops = |n: i64| [seed(), count(Value::Int(n), Some((4, 2))), connect("/seed", "/count", 0)];
    let Some(px) = run("iter_size_1", &ops(1), "/count") else { return };
    assert_eq!((px.width, px.height), (4, 2));
    assert_eq!(px.get(0, 0).map(|p| p[3]), Some(16.0));
    let Some(px) = run("iter_size_3", &ops(3), "/count") else { return };
    assert_eq!((px.width, px.height), (4, 2));
    assert_eq!(px.get(0, 0).map(|p| p[3]), Some(4.0));
}
