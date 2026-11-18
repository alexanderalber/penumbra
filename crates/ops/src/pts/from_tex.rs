//! `pts.from_tex`: one point per texel of a picture, or per cell of texels when the picture is
//! sampled down, TouchDesigner's TOP to POP. The picture's channels mean a colour on a flat
//! grid, a height that lifts the grid into a relief, a position, or a depth seen by a camera.
//!
//! The points form a grid of rows and columns (`dims`), row 0 at the top of the picture, so
//! `tex.from_pts` turns them back into a picture of the same layout. Every point gets `uv` (the
//! centre of its cell) and `weight` (the picture's alpha there; 0 where a depth picture holds
//! no depth), for a node that keeps only some of the points.

use penumbra_core::{Curve, Error, ParamSpec, Unit, Value};
use penumbra_gpu::Gpu;
use penumbra_gpu::points::Points;
use penumbra_gpu::points::program::{self, Decl, Program, Shape, Spec};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, JobStatus, NodeDesc, NodeTypes, Operator,
};
use penumbra_mesh::Bounds;

use super::generate::MAX_POINTS;
use super::program::{Own, picture};
use crate::{gpu, on_gpu};

pub const KIND: &str = "pts.from_tex";

/// Choices of `mode`, `sampling`, `filter` and `channel`.
pub const MODES: [&str; 4] = ["color", "height", "position", "depth"];
pub const SAMPLINGS: [&str; 2] = ["count", "step"];
pub const FILTERS: [&str; 2] = ["nearest", "average"];
pub const CHANNELS: [&str; 5] = ["luminance", "red", "green", "blue", "alpha"];

/// Most texels one point averages along each side; a larger cell takes this many, spread evenly.
const MOST_AVERAGED: u32 = 8;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            KIND,
            "One point per texel of a picture, or per cell when sampled down: coloured points on a grid, a relief lifted by brightness, positions stored in a picture, or a depth picture as a point cloud",
        )
        .param(
            ParamSpec::choice("mode", &MODES)
                .doc("What the picture's channels mean")
                .choice_doc("color", "the points lie on a flat grid in the picture's shape, upright and facing -y like pts.generate's grid. `color` is the picture's rgba as stored (premultiplied)")
                .choice_doc("height", "the grid of `color`, each point lifted towards -y by `channel` times `height`: an image as a relief of points")
                .choice_doc("position", "red, green and blue are x, y and z in metres, as a picture from tex.from_pts or a texel simulation holds them")
                .choice_doc("depth", "`channel` times `depth_scale` is the distance in metres along the view of a camera at the origin looking along +y, z up, with the vertical field of view `fov`. A depth of 0 gives weight 0"),
        )
        .param(
            ParamSpec::choice("sampling", &SAMPLINGS)
                .doc("How many points the picture gives")
                .choice_doc("count", "about `count` points, spread evenly in rows and columns in the picture's shape")
                .choice_doc("step", "a point for every `step` texels across and down; 1 is a point per texel"),
        )
        .param(
            ParamSpec::int("count", 20_000)
                .clamp(1.0, f64::from(MAX_POINTS))
                .range(100.0, 1e6)
                .curve(Curve::Log)
                .doc("About this many points; never more than the picture has texels. The viewer shows the count made")
                .applies_when("sampling", &["count"]),
        )
        .param(
            ParamSpec::int("step", 4)
                .clamp(1.0, 4096.0)
                .range(1.0, 64.0)
                .curve(Curve::Log)
                .doc("Texels from one point to the next, across and down: 4 makes a 1920x1080 picture 480x270 points")
                .applies_when("sampling", &["step"]),
        )
        .param(
            ParamSpec::choice("filter", &FILTERS)
                .doc("What a point takes from its cell of texels when the picture is sampled down")
                .choice_doc("nearest", "the texel at the cell's centre. Keeps positions and depths exact")
                .choice_doc("average", "the mean of the cell's texels, up to 8x8 of them: smoother colours, fewer flickering points in a moving picture. Blends positions and depths at edges"),
        )
        .param(
            ParamSpec::float("size", 1.0)
                .clamp(0.0, 1e4)
                .range(0.01, 10.0)
                .curve(Curve::Log)
                .unit(Unit::Metres)
                .doc("color and height: the width of the grid; its height follows the picture's shape")
                .applies_when("mode", &["color", "height"]),
        )
        .param(
            ParamSpec::float("height", 0.2)
                .range(-1.0, 1.0)
                .unit(Unit::Metres)
                .doc("height: how far a value of 1 in `channel` lifts a point towards -y")
                .applies_when("mode", &["height"]),
        )
        .param(
            ParamSpec::choice("channel", &CHANNELS)
                .doc("height and depth: the channel that holds the number")
                .choice_doc("luminance", "the brightness of red, green and blue (Rec. 709 weights)")
                .choice_doc("red", "red")
                .choice_doc("green", "green")
                .choice_doc("blue", "blue")
                .choice_doc("alpha", "alpha")
                .applies_when("mode", &["height", "depth"]),
        )
        .param(
            ParamSpec::float("fov", 60.0)
                .clamp(1.0, 170.0)
                .range(20.0, 120.0)
                .unit(Unit::Degrees)
                .doc("depth: the camera's vertical field of view. Depth cameras see about 45 to 60 degrees")
                .applies_when("mode", &["depth"]),
        )
        .param(
            ParamSpec::float("depth_scale", 1.0)
                .range(0.0, 100.0)
                .doc("depth: metres per unit of `channel`. A 16-bit depth picture in millimetres loads as 0 to 1 for 0 to 65.535 m: use 65.535")
                .applies_when("mode", &["depth"]),
        )
        .input("in", DataKind::Tex)
        .describe_input("in", "The picture: 8-bit, half floats or 32-bit floats")
        .output("out", DataKind::Pts)
        .describe_output("out", "The points in rows and columns, row 0 at the top of the picture, with `position`, `uv` and `weight`. color and height add `color` and `normal`"),
        || Box::new(FromTex::default()) as Box<dyn Operator>,
    )
    .warm_up(on_gpu(warm_up)))
}

/// Rows and columns of points for a picture of `w` by `h` texels.
pub fn cells(w: u32, h: u32, step: Option<u32>, count: u32) -> (u32, u32) {
    let (w, h) = (w.max(1), h.max(1));
    match step {
        Some(k) => {
            let k = k.max(1);
            (w.div_ceil(k), h.div_ceil(k))
        }
        None => {
            let aspect = f64::from(w) / f64::from(h);
            let cols = (f64::from(count.max(1)) * aspect).sqrt().round().clamp(1.0, f64::from(w)) as u32;
            let rows = (f64::from(cols) / aspect).round().clamp(1.0, f64::from(h)) as u32;
            (cols, rows)
        }
    }
}

/// What every mode shares: the cell of texels of each point, read as one value.
const CELL: &str = "
fn pts_uv() -> vec2f {
    let cells = vec2u(u32(max(params.cols, 1)), u32(max(params.rows, 1)));
    let cell = vec2u(index % cells.x, index / cells.x);
    return (vec2f(cell) + 0.5) / vec2f(cells);
}

fn pts_cell() -> vec4f {
    let size = vec2i(textureDimensions(pic0));
    let cells = vec2u(u32(max(params.cols, 1)), u32(max(params.rows, 1)));
    let cell = vec2f(vec2u(index % cells.x, index / cells.x));
    let per = vec2f(size) / vec2f(cells);
    if params.average == 0 {
        return textureLoad(pic0, clamp(vec2i((cell + 0.5) * per), vec2i(0), size - 1), 0);
    }
    let lo = clamp(vec2i(floor(cell * per)), vec2i(0), size - 1);
    let hi = clamp(vec2i(ceil((cell + 1.0) * per)) - 1, lo, size - 1);
    let n = hi - lo + 1;
    let k = min(n, vec2i(params.most));
    var sum = vec4f(0.0);
    for (var y = 0; y < k.y; y++) {
        for (var x = 0; x < k.x; x++) {
            sum += textureLoad(pic0, lo + vec2i(x, y) * n / k, 0);
        }
    }
    return sum / f32(k.x * k.y);
}

fn pts_channel(c: vec4f) -> f32 {
    var s = pn_luma(c.rgb);
    if params.channel == channel_red {
        s = c.r;
    } else if params.channel == channel_green {
        s = c.g;
    } else if params.channel == channel_blue {
        s = c.b;
    } else if params.channel == channel_alpha {
        s = c.a;
    }
    return s;
}

fn pts_flat(u: vec2f) -> vec3f {
    return vec3f((u.x - 0.5) * params.size.x, 0.0, (0.5 - u.y) * params.size.y);
}
";

/// The params the program reads.
fn program_params() -> Vec<ParamSpec> {
    vec![
        ParamSpec::int("cols", 1),
        ParamSpec::int("rows", 1),
        ParamSpec::int("average", 0),
        ParamSpec::int("most", i64::from(MOST_AVERAGED)),
        ParamSpec::vec2("size", [1.0, 1.0]),
        ParamSpec::float("height", 0.2),
        ParamSpec::choice("channel", &CHANNELS),
        ParamSpec::float("tan_half", 0.5),
        ParamSpec::float("depth_scale", 1.0),
    ]
}

/// The program of `mode`.
fn spec(mode: &str) -> Spec {
    let (body, coloured) = match mode {
        "height" => ("position = pts_flat(u) + vec3f(0.0, -params.height * pts_channel(c), 0.0);", true),
        "position" => ("position = c.xyz;", false),
        "depth" => (
            "let d = pts_channel(c) * params.depth_scale;
    let size = vec2f(textureDimensions(pic0));
    let t = params.tan_half;
    position = vec3f((2.0 * u.x - 1.0) * t * size.x / max(size.y, 1.0) * d, d, (1.0 - 2.0 * u.y) * t * d);
    weight = select(0.0, c.a, d > 0.0);",
            false,
        ),
        _ => ("position = pts_flat(u);", true),
    };
    let colour = if coloured { "color = c;\n    normal = vec3f(0.0, -1.0, 0.0);" } else { "" };
    let source = format!(
        "{CELL}
fn main() {{
    let c = pts_cell();
    let u = pts_uv();
    uv = u;
    weight = c.a;
    {colour}
    {body}
}}
"
    );
    let mut decls = vec![Decl::set("position"), Decl::set("uv"), Decl::set("weight")];
    if coloured {
        decls.extend([Decl::set("color"), Decl::set("normal")]);
    }
    Spec { label: KIND.into(), source, decls, params: program_params() }
}

/// Compiles the program of the default mode.
fn warm_up(gpu: &Gpu) -> Result<(), Error> {
    let generated = program::generate(&spec("color"), &Shape::default(), program::storage_budget(gpu))?;
    Program::cached(gpu, generated).map(|_| ())
}

#[derive(Default)]
struct FromTex {
    own: Own,
}

impl Operator for FromTex {
    fn gpu_bytes(&self) -> u64 {
        self.own.bytes()
    }

    fn trim(&mut self) {
        self.own.trim();
    }

    fn job_status(&self) -> Option<JobStatus> {
        Some(self.own.status())
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let tex = picture(cx, 0, "in")?;
        let (w, h) = tex.size();
        let step = match cx.text("sampling")? {
            "step" => Some(u32::try_from(cx.int("step")?.max(1)).unwrap_or(u32::MAX)),
            _ => None,
        };
        let count = u32::try_from(cx.int("count")?.max(1)).unwrap_or(MAX_POINTS);
        let (cols, rows) = cells(w, h, step, count);
        let n = u64::from(cols) * u64::from(rows);
        if n > u64::from(MAX_POINTS) {
            return Err(CookError::failed(format!(
                "{cols}x{rows} points are more than the {MAX_POINTS} allowed: choose a larger `step`, or `count`"
            )));
        }
        let n = n as u32;
        let mode = cx.text("mode")?;
        let size = cx.float("size")?;
        let aspect = f64::from(h) / f64::from(w.max(1));
        let tan_half = (cx.float("fov")?.to_radians() * 0.5).tan();
        let average = cx.text("filter")? == "average";
        let param = |name: &str| match name {
            "cols" => Some(Value::Int(i64::from(cols))),
            "rows" => Some(Value::Int(i64::from(rows))),
            "average" => Some(Value::Int(i64::from(average))),
            "most" => Some(Value::Int(i64::from(MOST_AVERAGED))),
            "size" => Some(Value::Vec2([size, size * aspect])),
            "tan_half" => Some(Value::Float(tan_half)),
            other => cx.param(other).cloned(),
        };
        let empty = Points::new(n, n, Vec::new())?;
        let out = self.own.run(cx, gpu, &spec(mode), &empty, false, &param, &[Some(tex)])?;
        let bounds = if mode == "color" {
            let (x, z) = (0.5 * size as f32, 0.5 * (size * aspect) as f32);
            Bounds::of([[-x, 0.0, -z], [x, 0.0, z]])
        } else {
            None
        };
        let out = out.with_bounds(self.own.bounds(bounds)).with_dims(Some([rows, cols, 1]));
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Pts, out)]))
    }
}
