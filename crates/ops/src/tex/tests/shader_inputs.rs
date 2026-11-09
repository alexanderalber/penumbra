//! The eight inputs of `tex.shader` on WARP: a shader reads `in7`, an unwired input reads as
//! transparent black wherever it sits, a later pass of `iterations` still reads a high input,
//! and the node reports the inputs its source reads, so the editor shows them.

use penumbra_core::Value;
use penumbra_graph::{Op, reads};

use super::{Rig, connect, create, gpu, scratch};

/// A 4 x 4 picture of one grey level, `level` in red, green and blue.
const FILL: &str = "// @param level float = 0.0
@fragment fn main(v: PnVertex) -> @location(0) vec4f { return vec4f(vec3f(params.level), 1.0); }";

/// in7's red and alpha, and the alpha of in3, which the tests leave unwired.
const SEVEN: &str = "@fragment fn main(v: PnVertex) -> @location(0) vec4f { \
    return vec4f(textureLoad(in7, vec2i(0), 0).r, textureLoad(in7, vec2i(0), 0).a, \
    textureLoad(in3, vec2i(0), 0).a, 1.0); }";

/// Red counts up by in6's red per pass; in0 is the pass before from the second pass on.
const STEP: &str = "@fragment fn main(v: PnVertex) -> @location(0) vec4f { \
    return vec4f(textureLoad(in0, vec2i(0), 0).r + textureLoad(in6, vec2i(0), 0).r, 0.0, 0.0, 1.0); }";

fn fill(path: &str, level: f64) -> Op {
    create(
        path,
        "tex.shader",
        &[
            ("file", "fill.wgsl".into()),
            ("resolution", "custom".into()),
            ("width", Value::Int(4)),
            ("height", Value::Int(4)),
            ("level", Value::Float(level)),
        ],
    )
}

fn shader(path: &str, file: &str, iterations: i64) -> Op {
    create(
        path,
        "tex.shader",
        &[
            ("file", file.into()),
            ("resolution", "custom".into()),
            ("width", Value::Int(4)),
            ("height", Value::Int(4)),
            ("iterations", Value::Int(iterations)),
        ],
    )
}

fn setup(name: &str, ops: &[Op]) -> Option<(Rig, penumbra_core::scratch::ScratchDir)> {
    let gpu = gpu()?;
    let dir = scratch(name);
    for (file, text) in [("fill.wgsl", FILL), ("seven.wgsl", SEVEN), ("step.wgsl", STEP)] {
        std::fs::write(dir.join(file), text).unwrap();
    }
    let rig = Rig::new(gpu, Some(&dir), ops);
    Some((rig, dir))
}

#[test]
fn a_shader_reads_in7_and_an_unwired_input_is_transparent_black() {
    let ops = [fill("/a", 0.5), shader("/s", "seven.wgsl", 1), connect("/a", "/s", 7)];
    let Some((mut rig, _dir)) = setup("inputs_in7", &ops) else { return };
    // The source first: a shader whose in7 has nothing yet runs with it black.
    rig.pixels("/a");
    let px = rig.pixels("/s");
    // in7's red and alpha as wired; in3 is unwired and reads alpha 0, as in1 to in3 always did.
    assert_eq!(px.get(0, 0), Some([0.5, 1.0, 0.0, 1.0]));

    // Unwired, in7 reads as transparent black too.
    let ops = [shader("/s", "seven.wgsl", 1)];
    let Some((mut rig, _dir)) = setup("inputs_in7_unwired", &ops) else { return };
    assert_eq!(rig.pixels("/s").get(0, 0), Some([0.0, 0.0, 0.0, 1.0]));
}

#[test]
fn every_pass_of_iterations_reads_a_high_input() {
    let ops = [fill("/k", 0.25), shader("/s", "step.wgsl", 3), connect("/k", "/s", 6)];
    let Some((mut rig, _dir)) = setup("inputs_iterations", &ops) else { return };
    rig.pixels("/k");
    // Pass 0 reads the unwired in0 (0), passes 1 and 2 the pass before: 3 x 0.25.
    assert_eq!(rig.pixels("/s").get(0, 0).map(|p| p[0]), Some(0.75));
}

#[test]
fn the_node_reports_the_inputs_its_source_reads() {
    let ops = [shader("/seven", "seven.wgsl", 1), shader("/step", "step.wgsl", 1), fill("/fill", 0.0)];
    let Some((mut rig, _dir)) = setup("inputs_reads", &ops) else { return };
    for path in ["/seven", "/step", "/fill"] {
        rig.pixels(path);
    }
    let read = |rig: &Rig, path: &str| rig.cooker.status(rig.id(path)).unwrap().reads;
    assert_eq!(read(&rig, "/seven"), 1 << 3 | 1 << 7);
    assert_eq!(read(&rig, "/step"), 1 | 1 << 6);
    assert_eq!(read(&rig, "/fill"), 0);

    // The editor's rule: every input up to the highest one wired or read, and one more, up to 8.
    let desc = rig.types.desc("tex.shader").unwrap();
    let shown = |mask: u64, wired: &[usize]| desc.shown_inputs(|i| wired.contains(&i) || reads(mask, i));
    assert_eq!(shown(0, &[]), [0], "a generator that reads nothing");
    assert_eq!(shown(0, &[0]), [0, 1]);
    assert_eq!(shown(1 | 1 << 6, &[0]), (0..8).collect::<Vec<_>>(), "in6 read: in0 to in7");
    assert_eq!(shown(1 << 3 | 1 << 7, &[]), (0..8).collect::<Vec<_>>(), "never more than 8");
    assert_eq!(shown(1 << 2, &[0]), [0, 1, 2, 3], "in2 read, unwired");
    assert_eq!(desc.inputs.len(), penumbra_gpu::pass::INPUTS);
}
