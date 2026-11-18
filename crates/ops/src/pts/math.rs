//! `pts.math`: an attribute computed per point from a typed expression, such as
//! `position.z + sin(time + index01 * 6.283) * 0.1`. One node covers what TouchDesigner spreads
//! over Math, Math Mix, Math Combine, Limit, ReRange, Normalize and Trig POPs.
//!
//! The expression is WGSL. The attributes it names are found and read; TouchDesigner's names
//! (`P`, `Color`) mean penumbra's. It compiles off the cook thread; one that does not compile
//! leaves the last good one running, with the compiler's message as the node's warning.

use penumbra_core::{Error, ParamSpec};
use penumbra_gpu::points::program::{self, Decl, Shape, Spec};
use penumbra_gpu::points::{AttrType, Points, canonical, default_of};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, JobStatus, NodeDesc, NodeTypes, Operator,
};

use super::program::{Compiling, attr_type, input, type_param};
use crate::gpu;

pub const KIND: &str = "pts.math";

/// The values an expression can name besides attributes.
const VALUES: [&str; 4] = ["a", "b", "c", "d"];

/// Longest expression.
const MAX_EXPRESSION: usize = 4096;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    let mut desc = NodeDesc::new(
        KIND,
        "Computes an attribute per point from an expression you type, such as position.z + sin(time) * 0.1: arithmetic, limits, ranges, trigonometry",
    )
    .param(
        ParamSpec::text("expression", "position")
            .lines(2)
            .doc("A WGSL expression, worked out once per point. It can name attributes (position, color, P, Color), index, count, index01 (0 at the first point, 1 at the last), time, dt, frame, rand(k) and rand3(k), a, b, c and d, and WGSL's functions: sin, clamp, mix, smoothstep, length, normalize, ... One number fills every number of the result"),
    )
    .param(
        ParamSpec::text("result", "position")
            .doc("The attribute the expression goes into, TouchDesigner's names too. A part of it after a dot: position.z, color.rgb. A new attribute is made"),
    )
    .param(type_param());
    for (name, doc) in VALUES.iter().zip([
        "A number the expression reads as a. Bind it to a channel to steer the expression",
        "A number the expression reads as b",
        "A number the expression reads as c",
        "A number the expression reads as d",
    ]) {
        desc = desc.param(ParamSpec::float(name, 0.0).range(-1.0, 1.0).more().doc(doc));
    }
    types.register(FnNodeType::new(
        desc.input("in", DataKind::Pts).output("out", DataKind::Pts).describe_output(
            "out",
            "The points with the result attribute computed; the others pass on unchanged",
        ),
        || Box::new(Math::default()) as Box<dyn Operator>,
    ))
}

/// `expression` with TouchDesigner's attribute names replaced by penumbra's, and the attributes
/// it names: those `is_attr` accepts.
pub fn scan(expression: &str, is_attr: impl Fn(&str) -> bool) -> (String, Vec<String>) {
    let mut out = String::with_capacity(expression.len());
    let mut names: Vec<String> = Vec::new();
    let mut chars = expression.char_indices().peekable();
    // The last character that was no space: after a dot comes a member, no attribute.
    let mut last = ' ';
    while let Some((start, c)) = chars.next() {
        if c.is_ascii_alphabetic() || c == '_' || c.is_ascii_digit() {
            let mut end = start + c.len_utf8();
            while let Some((i, d)) = chars.peek().copied() {
                // A number takes its point and exponent along, so `1.5e3` stays one word.
                let number = c.is_ascii_digit() && d == '.';
                if d.is_ascii_alphanumeric() || d == '_' || number {
                    end = i + d.len_utf8();
                    chars.next();
                } else {
                    break;
                }
            }
            let word = expression.get(start..end).unwrap_or("");
            if c.is_ascii_digit() || last == '.' {
                out.push_str(word);
            } else {
                let name = canonical(word);
                if is_attr(name) && !names.iter().any(|n| n == name) {
                    names.push(name.to_string());
                }
                out.push_str(if is_attr(name) { name } else { word });
            }
            last = word.chars().last().unwrap_or(' ');
        } else {
            out.push(c);
            if !c.is_whitespace() {
                last = c;
            }
        }
    }
    (out, names)
}

/// `result` split into the attribute and the part of it after a dot, as component indices.
fn split_result(result: &str) -> Result<(&str, Vec<usize>), Error> {
    let (name, part) = match result.trim().split_once('.') {
        Some((n, p)) => (n, Some(p.trim())),
        None => (result.trim(), None),
    };
    let Some(part) = part else { return Ok((canonical(name), Vec::new())) };
    let index = |c: char| "xyzw".find(c).or_else(|| "rgba".find(c));
    let picks: Option<Vec<usize>> = part.chars().map(index).collect();
    let mixed = part.chars().any(|c| "xyzw".contains(c)) && part.chars().any(|c| "rgba".contains(c));
    match picks {
        Some(p) if !p.is_empty() && p.len() <= 4 && !mixed => {
            let mut seen = p.clone();
            seen.sort_unstable();
            seen.dedup();
            if seen.len() != p.len() {
                return Err(Error::new(format!("`{result}` names a number twice")));
            }
            Ok((canonical(name), p))
        }
        _ => Err(Error::new(format!(
            "`{result}`: after the dot come up to four of x, y, z, w (or r, g, b, a), such as position.z or color.rgb"
        ))),
    }
}

/// The source of `fn main` for `expression` (already scanned) into `name` of type `ty`, the
/// numbers `picks` of it.
fn body(expression: &str, name: &str, ty: AttrType, picks: &[usize]) -> Result<String, Error> {
    let width = ty.components() as usize;
    if let Some(bad) = picks.iter().find(|p| **p >= width) {
        let what = if width == 1 { "one number" } else { "fewer numbers" };
        let c = ["x", "y", "z", "w"].get(*bad).copied().unwrap_or("?");
        return Err(Error::new(format!("`{name}` is {} and holds {what}; it has no .{c}", ty.wgsl())));
    }
    let scalar = match ty {
        AttrType::U32 => "u32",
        AttrType::I32 => "i32",
        _ => "f32",
    };
    let value_ty = match picks.len() {
        0 => ty.wgsl().to_string(),
        1 => scalar.to_string(),
        n => format!("vec{n}f"),
    };
    let mut s = String::from("fn main() {\n");
    for v in VALUES {
        s.push_str(&format!("    let {v} = params.{v};\n"));
    }
    s.push_str(&format!("    let pts_v = {value_ty}(\n{expression}\n    );\n"));
    const AXES: [&str; 4] = ["x", "y", "z", "w"];
    match picks {
        [] => s.push_str(&format!("    {name} = pts_v;\n")),
        [one] => s.push_str(&format!("    {name}.{} = pts_v;\n", AXES.get(*one).copied().unwrap_or("x"))),
        many => {
            for (j, p) in many.iter().enumerate() {
                let to = AXES.get(*p).copied().unwrap_or("x");
                let from = AXES.get(j).copied().unwrap_or("x");
                s.push_str(&format!("    {name}.{to} = pts_v.{from};\n"));
            }
        }
    }
    s.push_str("}\n");
    Ok(s)
}

/// The program for the node's params over `points`. The outer `Err` is the node's, the inner
/// one the expression's.
fn spec(cx: &CookContext<'_>, points: &Points) -> Result<Result<Spec, Error>, CookError> {
    let expression = cx.text("expression")?;
    let result = cx.text("result")?;
    let chosen = cx.text("type")?;
    Ok(make(expression, result, chosen, points))
}

fn make(expression: &str, result: &str, chosen: &str, points: &Points) -> Result<Spec, Error> {
    if expression.trim().is_empty() {
        return Err(Error::new("the expression is empty"));
    }
    if expression.len() > MAX_EXPRESSION {
        return Err(Error::new(format!("the expression is longer than {MAX_EXPRESSION} characters")));
    }
    let (name, picks) = split_result(result)?;
    program::check_name(name).map_err(|e| Error::new(format!("result: {}", e.0)))?;
    let ty = attr_type(points, name, chosen);
    let is_attr = |n: &str| points.attr(n).is_some() || default_of(n).is_some();
    let (expression, reads) = scan(expression, is_attr);
    let mut decls: Vec<Decl> = reads.iter().map(|n| Decl::read(n)).collect();
    decls.push(Decl::write(name).ty(ty));
    Ok(Spec {
        label: KIND.into(),
        source: body(&expression, name, ty, &picks)?,
        decls,
        params: VALUES.iter().map(|v| ParamSpec::float(v, 0.0)).collect(),
    })
}

#[derive(Default)]
struct Math {
    compiling: Compiling,
}

impl Operator for Math {
    fn gpu_bytes(&self) -> u64 {
        self.compiling.bytes()
    }

    fn trim(&mut self) {
        self.compiling.trim();
    }

    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        self.compiling.poll();
        let gpu = gpu(cx)?;
        let points = input(cx)?;
        let made = spec(cx, points)?
            .and_then(|s| program::generate(&s, &Shape::of(points), program::storage_budget(gpu)));
        let made_error = match made {
            Ok(g) => {
                self.compiling.want(cx, gpu, g);
                None
            }
            Err(e) => Some(e.0),
        };
        let problem = made_error.or(self.compiling.error().map(str::to_string));
        let Some(program) = self.compiling.current(points) else {
            return Err(match problem {
                Some(e) => CookError::failed(e),
                None => CookError::Pending,
            });
        };
        if let Some(e) = problem {
            cx.warn(format!("{e}\n(running the last expression that compiled)"));
        }
        let param = |name: &str| cx.param(name).cloned();
        let out = self.compiling.run(cx, gpu, &program, points, &param, &[])?;
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Pts, out)]))
    }

    fn job_status(&self) -> Option<JobStatus> {
        Some(self.compiling.status())
    }

    fn time_dependent(&self) -> Option<bool> {
        Some(self.compiling.latest().is_some_and(|p| p.uses_time()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expressions_name_their_attributes() {
        let known = |n: &str| ["position", "color", "heat"].contains(&n);
        let (e, names) =
            scan("P.z + sin(time + index01 * 6.28) * heat + Color.r * 1.5e3 + vec3f(1.0).x", known);
        assert_eq!(e, "position.z + sin(time + index01 * 6.28) * heat + color.r * 1.5e3 + vec3f(1.0).x");
        assert_eq!(names, ["position", "heat", "color"]);
        // A member after a dot is no attribute, even when it has an attribute's name.
        let (e, names) = scan("foo . heat + x", known);
        assert_eq!((e.as_str(), names.len()), ("foo . heat + x", 0));

        assert_eq!(split_result("P").unwrap(), ("position", vec![]));
        assert_eq!(split_result("Color.rgb").unwrap(), ("color", vec![0, 1, 2]));
        assert_eq!(split_result(" position.zx ").unwrap(), ("position", vec![2, 0]));
        assert!(split_result("position.q").is_err());
        assert!(split_result("position.xx").is_err());
        assert!(split_result("color.xg").is_err());
        assert!(body("1.0", "scale", AttrType::F32, &[1]).unwrap_err().0.contains("one number"));
        let b = body("x", "color", AttrType::Vec4, &[0, 2]).unwrap();
        assert!(
            b.contains("pts_v = vec2f(\nx\n")
                && b.contains("color.x = pts_v.x;")
                && b.contains("color.z = pts_v.y;")
        );
    }
}
