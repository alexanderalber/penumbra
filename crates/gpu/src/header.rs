//! The header of a shader file: comment lines that declare its parameters and metadata, so the
//! UI can show controls for them.
//!
//! ```wgsl
//! // @param scale float = 4 1..32 -- cells per image height
//! // @param tint color = [1, 0.5, 0.2, 1]
//! // @param octaves int = 3 1..8
//! // @param invert bool
//! // @param mode choice(add|multiply|screen) = multiply
//! // @density 0.5
//! ```
//!
//! `@param <name> <kind> [= <default>] [<min>..<max>] [-- <doc>]`. Kinds: `float`, `int`, `bool`,
//! `vec2`, `vec3`, `vec4`, `color`, `choice(a|b|c)`. The default is JSON (`1.5`, `[1, 0, 0, 1]`,
//! `true`) or, for a choice, a bare word; without one it is zero, false or the first choice.
//! A range is a soft range (slider only). `@density` is the resolution the shader needs of its
//! inputs per unit of its output's (default 1; 2 when it magnifies by 2, 0.5 when it shrinks),
//! for `res.analyze`. Lines may appear anywhere in the file.

use penumbra_core::{Error, ParamSpec, Value};

/// What a shader file declares about itself.
#[derive(Clone, Debug, PartialEq)]
pub struct Header {
    pub params: Vec<ParamSpec>,
    pub density: f64,
}

/// Parses every `// @...` line of `source`. Errors name the line.
pub fn parse(source: &str) -> Result<Header, Error> {
    let mut header = Header { params: Vec::new(), density: 1.0 };
    for (i, line) in source.lines().enumerate() {
        let Some(rest) = line.trim_start().strip_prefix("//") else { continue };
        let rest = rest.trim();
        let ctx = || format!("line {}", i + 1);
        if !rest.starts_with('@') {
            continue;
        }
        let (word, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        if word == "@param" {
            let decl = args;
            let spec = param(decl).map_err(|e| e.context(ctx()))?;
            if header.params.iter().any(|p| p.name == spec.name) {
                return Err(Error::new(format!("param `{}` declared twice", spec.name)).context(ctx()));
            }
            header.params.push(spec);
        } else if word == "@density" {
            let d = args;
            header.density =
                d.trim().parse::<f64>().ok().filter(|d| d.is_finite() && *d > 0.0).ok_or_else(|| {
                    Error::new(format!("@density wants a positive number, got `{}`", d.trim())).context(ctx())
                })?;
        } else {
            return Err(
                Error::new(format!("unknown header `{word}`; known: @param, @density")).context(ctx())
            );
        }
    }
    Ok(header)
}

fn param(decl: &str) -> Result<ParamSpec, Error> {
    let (decl, doc) = match decl.split_once("--") {
        Some((d, doc)) => (d, doc.trim()),
        None => (decl, ""),
    };
    let decl = decl.trim();
    let (head, default) = match decl.split_once('=') {
        Some((h, d)) => (h.trim(), Some(d.trim())),
        None => (decl, None),
    };
    // A range is the last token of whatever follows the kind (or the default).
    let (head, default, range) = {
        let tail = default.unwrap_or(head);
        let (before, last) = tail.rsplit_once(char::is_whitespace).unwrap_or(("", tail));
        match range(last) {
            Some(r) if default.is_some() => (head, Some(before.trim()), Some(r)),
            Some(r) => (before.trim(), None, Some(r)),
            None => (head, default, None),
        }
    };
    let mut words = head.split_whitespace();
    let (Some(name), Some(kind), None) = (words.next(), words.next(), words.next()) else {
        return Err(Error::new("expected `@param <name> <kind> [= default] [min..max] [-- doc]`"));
    };
    let default = default.filter(|d| !d.is_empty());
    let mut spec = match kind {
        "float" => ParamSpec::float(name, 0.0),
        "int" => ParamSpec::int(name, 0),
        "bool" => ParamSpec::bool(name, false),
        "vec2" => ParamSpec::vec2(name, [0.0; 2]),
        "vec3" => ParamSpec::vec3(name, [0.0; 3]),
        "vec4" => ParamSpec::vec4(name, [0.0; 4]),
        "color" => ParamSpec::color(name, [0.0, 0.0, 0.0, 1.0]),
        k => match k.strip_prefix("choice(").and_then(|c| c.strip_suffix(')')) {
            Some(list) => {
                let choices: Vec<&str> = list.split('|').map(str::trim).collect();
                ParamSpec::choice(name, &choices)
            }
            None => {
                return Err(Error::new(format!(
                    "unknown kind `{k}`; kinds: float, int, bool, vec2, vec3, vec4, color, choice(a|b)"
                )));
            }
        },
    };
    if let Some(d) = default {
        let value = match serde_json::from_str::<Value>(d) {
            Ok(v) => v,
            Err(_) => Value::Text(d.to_string()),
        };
        spec.default = spec.check(&value)?;
    }
    if let Some((min, max)) = range {
        spec = spec.range(min, max);
    }
    spec = spec.doc(doc);
    spec.validate_spec()?;
    Ok(spec)
}

fn range(token: &str) -> Option<(f64, f64)> {
    let (a, b) = token.split_once("..")?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use penumbra_core::ParamKind;

    #[test]
    fn declarations() {
        let h = parse(
            "// @param scale float = 4 1..32 -- cells per image height\n\
             fn x() {}\n\
             // @param tint color = [1, 0.5, 0.2, 1]\n\
             //@param octaves int 1..8\n\
             // @param invert bool\n\
             // @param mode choice(add|multiply) = multiply\n\
             // @density 0.5\n\
             // a normal comment",
        )
        .unwrap();
        assert_eq!(h.density, 0.5);
        let names: Vec<&str> = h.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["scale", "tint", "octaves", "invert", "mode"]);
        let scale = &h.params[0];
        assert_eq!(scale.default, Value::Float(4.0));
        assert_eq!(scale.range.map(|r| (r.min, r.max, r.hard)), Some((1.0, 32.0, false)));
        assert_eq!(scale.doc, "cells per image height");
        assert_eq!(h.params[1].default, Value::Vec4([1.0, 0.5, 0.2, 1.0]));
        assert_eq!(h.params[2].default, Value::Int(0));
        assert!(h.params[2].range.is_some());
        assert_eq!(h.params[4].kind, ParamKind::Choice(vec!["add".into(), "multiply".into()]));
        assert_eq!(h.params[4].default, Value::Text("multiply".into()));
    }

    #[test]
    fn mistakes_name_the_line() {
        for (src, needle) in [
            ("\n// @param x quaternion", "line 2"),
            ("// @param x float = abc", "line 1"),
            ("// @param x float\n// @param x int", "twice"),
            ("// @density -1", "positive"),
            ("// @paramx float", "unknown header"),
            ("// @param mode choice(a|b) = c", "must be one of"),
        ] {
            let e = parse(src).err().unwrap_or_else(|| panic!("{src} parsed"));
            assert!(e.0.contains(needle), "{src}: {e}");
        }
    }
}
