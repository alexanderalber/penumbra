//! `pts.lookup_tex`: each point samples a picture and writes what it finds into an attribute,
//! TouchDesigner's Lookup Texture POP. The place in the picture comes from an attribute: `uv`
//! as it is, or a position projected onto a plane, with a box of space mapped onto the picture.
//!
//! Texels are read with `textureLoad` and filtered in the program, so every picture format
//! works, edges follow `extend`, and linear filtering blends across a repeating edge correctly.

use penumbra_core::{Error, ParamSpec, Value};
use penumbra_gpu::Gpu;
use penumbra_gpu::points::program::{self, Decl, Program, Spec};
use penumbra_gpu::points::{AttrType, Points};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};
use penumbra_mesh::Bounds;

use super::program::{
    COMBINE, Own, attr_name, attr_type, combine, generated_shape, input, operation_param, picture, type_param,
};
use crate::{gpu, on_gpu};

pub const KIND: &str = "pts.lookup_tex";

/// Choices of `plane`, `fit`, `filter`, `extend` and `channels`.
pub const PLANES: [&str; 3] = ["xz", "xy", "yz"];
pub const FITS: [&str; 2] = ["bounds", "box"];
pub const FILTERS: [&str; 2] = ["linear", "nearest"];
pub const EXTENDS: [&str; 4] = ["clamp", "repeat", "mirror", "zero"];
pub const CHANNELS: [&str; 7] = ["rgba", "rgb", "luminance", "red", "green", "blue", "alpha"];

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            KIND,
            "Each point takes a value from a picture: colours from an image or a video, sizes or heights from brightness. Where it looks comes from `uv` or the point's position",
        )
        .param(
            ParamSpec::text("lookup", "uv")
                .doc("The attribute that says where each point looks, TouchDesigner's names too (Tex, P). Two numbers are the place in the picture: 0, 0 top left, 1, 1 bottom right. Three numbers are a place in space, projected onto `plane`"),
        )
        .param(
            ParamSpec::choice("plane", &PLANES)
                .doc("A lookup of three numbers: the plane the picture lies in. The picture's top is towards the plane's second axis")
                .choice_doc("xz", "upright, facing -y, as a picture seen from the front: x to the right, z up")
                .choice_doc("xy", "flat, seen from above: x to the right, y up")
                .choice_doc("yz", "upright, facing x, seen from the side: y to the right, z up"),
        )
        .param(
            ParamSpec::choice("fit", &FITS)
                .doc("A lookup of three numbers: the box of space the picture covers")
                .choice_doc("bounds", "the box around the points, where the node before knows it; else the box of `center` and `extent`, with a warning")
                .choice_doc("box", "the box of `center` and `extent`"),
        )
        .param(
            ParamSpec::vec3("center", [0.0; 3])
                .range(-10.0, 10.0)
                .doc("fit box: the middle of the box, in metres")
                .applies_when("fit", &["box"]),
        )
        .param(
            ParamSpec::vec3("extent", [1.0; 3])
                .range(0.0, 10.0)
                .doc("fit box: the size of the box along x, y and z, in metres; the two axes of `plane` count")
                .applies_when("fit", &["box"]),
        )
        .param(
            ParamSpec::choice("filter", &FILTERS)
                .doc("How a place between texels is read")
                .choice_doc("linear", "blended from the four nearest texels: smooth")
                .choice_doc("nearest", "the texel the place falls in: exact values, blocky"),
        )
        .param(
            ParamSpec::choice("extend", &EXTENDS)
                .doc("What a place outside the picture (below 0 or above 1) reads")
                .choice_doc("clamp", "the nearest edge texel")
                .choice_doc("repeat", "the picture again, tiled")
                .choice_doc("mirror", "the picture again, flipped at every edge")
                .choice_doc("zero", "transparent black"),
        )
        .param(
            ParamSpec::choice("channels", &CHANNELS)
                .doc("What the picture gives the attribute. An attribute of one number takes luminance unless one channel is chosen")
                .choice_doc("rgba", "all four channels")
                .choice_doc("rgb", "red, green and blue; a colour keeps its alpha")
                .choice_doc("luminance", "the brightness of red, green and blue (Rec. 709) in every number; a colour keeps its alpha")
                .choice_doc("red", "red in every number")
                .choice_doc("green", "green in every number")
                .choice_doc("blue", "blue in every number")
                .choice_doc("alpha", "alpha in every number"),
        )
        .param(
            ParamSpec::vec2("from", [0.0, 1.0])
                .range(-1.0, 2.0)
                .doc("The range of picture values that maps onto `to`: with from 0, 1 and to 1, 3 a black texel gives 1 and a white one 3")
                .more(),
        )
        .param(ParamSpec::vec2("to", [0.0, 1.0]).range(-1.0, 4.0).doc("The range `from` maps onto").more())
        .param(
            ParamSpec::text("attribute", "color")
                .doc("The attribute the value goes into, TouchDesigner's names too (Color, PointScale, P). A new attribute is made"),
        )
        .param(operation_param("set", "the picture's value"))
        .param(type_param())
        .input("in", DataKind::Pts)
        .input("picture", DataKind::Tex)
        .describe_input("in", "The points")
        .describe_input("picture", "The picture the points sample: 8-bit, half floats or 32-bit floats")
        .output("out", DataKind::Pts)
        .describe_output("out", "The points with the attribute set from the picture; the others pass on unchanged"),
        || Box::new(LookupTex::default()) as Box<dyn Operator>,
    )
    .warm_up(on_gpu(warm_up)))
}

/// Reading the picture: texels with the edge rule, filtering, channels and the range.
const SAMPLE: &str = "
fn pts_texel(at: vec2i, size: vec2i) -> vec4f {
    var p = at;
    if params.extend == extend_zero {
        if any(p < vec2i(0)) || any(p >= size) {
            return vec4f(0.0);
        }
    } else if params.extend == extend_repeat {
        p = ((p % size) + size) % size;
    } else if params.extend == extend_mirror {
        let period = 2 * size;
        let m = ((p % period) + period) % period;
        p = select(m, period - 1 - m, m >= size);
    } else {
        p = clamp(p, vec2i(0), size - 1);
    }
    return textureLoad(pic0, p, 0);
}

fn pts_sample(uv: vec2f) -> vec4f {
    let size = vec2i(textureDimensions(pic0));
    // Far outside, whole numbers lose their meaning; a NaN reads as the top left.
    let x = clamp(select(vec2f(0.0), uv, uv == uv), vec2f(-1e6), vec2f(1e6)) * vec2f(size);
    if params.linear == 0 {
        return pts_texel(vec2i(floor(x)), size);
    }
    let f = x - 0.5;
    let i = vec2i(floor(f));
    let t = f - floor(f);
    let top = mix(pts_texel(i, size), pts_texel(i + vec2i(1, 0), size), t.x);
    let bottom = mix(pts_texel(i + vec2i(0, 1), size), pts_texel(i + vec2i(1, 1), size), t.x);
    return mix(top, bottom, t.y);
}

fn pts_pick(c: vec4f) -> vec4f {
    var v = c;
    if params.channels == channels_luminance {
        v = vec4f(pn_luma(c.rgb));
    } else if params.channels == channels_red {
        v = vec4f(c.r);
    } else if params.channels == channels_green {
        v = vec4f(c.g);
    } else if params.channels == channels_blue {
        v = vec4f(c.b);
    } else if params.channels == channels_alpha {
        v = vec4f(c.a);
    }
    let span = params.src.y - params.src.x;
    let k = select(0.0, (params.dst.y - params.dst.x) / span, span != 0.0);
    return params.dst.x + (v - params.src.x) * k;
}
";

/// The params the program reads. It reads `from` and `to` as `src` and `dst`, and the plane and
/// box as the rows of a map from space to the picture.
fn program_params() -> Vec<ParamSpec> {
    vec![
        ParamSpec::choice("extend", &EXTENDS),
        ParamSpec::int("linear", 1),
        ParamSpec::choice("channels", &CHANNELS),
        ParamSpec::vec2("src", [0.0, 1.0]),
        ParamSpec::vec2("dst", [0.0, 1.0]),
        ParamSpec::vec4("u_axis", [1.0, 0.0, 0.0, 0.5]),
        ParamSpec::vec4("v_axis", [0.0, 0.0, -1.0, 0.5]),
        operation_param("set", "the picture's value"),
    ]
}

/// The WGSL for the place in the picture an attribute of type `ty` gives.
fn coordinate(name: &str, ty: AttrType) -> String {
    let projected = |q: &str| format!("vec2f(dot({q}, params.u_axis), dot({q}, params.v_axis))");
    match ty {
        AttrType::F32 => format!("vec2f({name}, 0.5)"),
        AttrType::U32 | AttrType::I32 => format!("vec2f(f32({name}), 0.5)"),
        AttrType::Vec2 => name.to_string(),
        AttrType::Vec3 => projected(&format!("vec4f({name}, 1.0)")),
        AttrType::Vec4 => projected(&format!("vec4f({name}.xyz, 1.0)")),
    }
}

/// The program that samples at `lookup` and puts the value into `target`.
fn spec(lookup: &str, lookup_ty: AttrType, target: &str, ty: AttrType, rgb_only: bool) -> Spec {
    let source = format!(
        "{SAMPLE}{COMBINE}
fn main() {{
    let pts_v = pts_pick(pts_sample({}));
    {}
}}
",
        coordinate(lookup, lookup_ty),
        combine(target, ty, "pts_v", rgb_only),
    );
    Spec {
        label: KIND.into(),
        source,
        decls: vec![Decl::read(lookup), Decl::write(target).ty(ty)],
        params: program_params(),
    }
}

/// Compiles the program of the defaults over the points `pts.generate` makes.
fn warm_up(gpu: &Gpu) -> Result<(), Error> {
    let spec = spec("uv", AttrType::Vec2, "color", AttrType::Vec4, false);
    let generated = program::generate(&spec, &generated_shape(), program::storage_budget(gpu))?;
    Program::cached(gpu, generated).map(|_| ())
}

/// The rows `u` and `v` of the map from a point in space to a place in the picture: the box
/// `b` spans 0..1 on the plane's two axes, its top towards the second axis.
pub fn plane_map(plane: &str, b: Bounds) -> ([f64; 4], [f64; 4]) {
    let (a, c) = match plane {
        "xy" => (0, 1),
        "yz" => (1, 2),
        _ => (0, 2),
    };
    let span = |k: usize| {
        let s = f64::from(b.max.get(k).copied().unwrap_or(1.0) - b.min.get(k).copied().unwrap_or(0.0));
        if s.abs() < 1e-6 { 1e-6 } else { s }
    };
    let low = |k: usize| f64::from(b.min.get(k).copied().unwrap_or(0.0));
    let mut u = [0.0; 4];
    let mut v = [0.0; 4];
    if let Some(x) = u.get_mut(a) {
        *x = 1.0 / span(a);
    }
    u[3] = -low(a) / span(a);
    if let Some(x) = v.get_mut(c) {
        *x = -1.0 / span(c);
    }
    v[3] = 1.0 + low(c) / span(c);
    (u, v)
}

/// The numbers of a vec3 param.
fn vec3(cx: &CookContext<'_>, name: &str) -> Result<[f32; 3], CookError> {
    match cx.param(name).and_then(Value::components) {
        Some(&[x, y, z]) => Ok([x as f32, y as f32, z as f32]),
        _ => Err(CookError::failed(format!("`{name}` must be three numbers"))),
    }
}

#[derive(Default)]
struct LookupTex {
    own: Own,
}

impl Operator for LookupTex {
    fn gpu_bytes(&self) -> u64 {
        self.own.bytes()
    }

    fn trim(&mut self) {
        self.own.trim();
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = gpu(cx)?;
        let points: &Points = input(cx)?;
        let tex = picture(cx, 1, "picture")?;
        let lookup = attr_name(cx, "lookup")?;
        let target = attr_name(cx, "attribute")?;
        let ty = attr_type(points, target, cx.text("type")?);
        let lookup_ty = attr_type(points, lookup, "auto");
        if points.attr(lookup).is_none() {
            cx.warn(format!("the points have no `{lookup}`: every point looks at the same place"));
        }
        let scalar = matches!(ty, AttrType::F32 | AttrType::U32 | AttrType::I32);
        let mut channels = cx.text("channels")?;
        if scalar && matches!(channels, "rgba" | "rgb") {
            channels = "luminance";
        }
        let rgb_only = channels != "rgba";

        let spatial = matches!(lookup_ty, AttrType::Vec3 | AttrType::Vec4);
        let fixed = || -> Result<Option<Bounds>, CookError> {
            let (c, e) = (vec3(cx, "center")?, vec3(cx, "extent")?);
            let h = e.map(|v| 0.5 * v.abs());
            Ok(Bounds::of([[c[0] - h[0], c[1] - h[1], c[2] - h[2]], [c[0] + h[0], c[1] + h[1], c[2] + h[2]]]))
        };
        let (u_axis, v_axis) = if spatial {
            let b = match (cx.text("fit")?, points.bounds()) {
                ("bounds", Some(b)) => Some(b),
                ("bounds", None) => {
                    cx.warn("the box around the points is not known here: the box of `center` and `extent` is used. A node that moves points without a known reach (pts.math, pts.shader) drops the box");
                    fixed()?
                }
                _ => fixed()?,
            };
            let b = b.unwrap_or(Bounds { min: [-0.5; 3], max: [0.5; 3] });
            plane_map(cx.text("plane")?, b)
        } else {
            ([1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0])
        };
        let spec = spec(lookup, lookup_ty, target, ty, rgb_only);
        let param = |name: &str| match name {
            "linear" => Some(Value::Int(i64::from(cx.text("filter").ok()? == "linear"))),
            "channels" => Some(Value::Text(channels.to_string())),
            "src" => cx.param("from").cloned(),
            "dst" => cx.param("to").cloned(),
            "u_axis" => Some(Value::Vec4(u_axis)),
            "v_axis" => Some(Value::Vec4(v_axis)),
            other => cx.param(other).cloned(),
        };
        let out = self.own.run(cx, gpu, &spec, points, false, &param, &[Some(tex)])?;
        let bounds = if target == "position" { None } else { points.bounds() };
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Pts, out.with_bounds(bounds))]))
    }
}
