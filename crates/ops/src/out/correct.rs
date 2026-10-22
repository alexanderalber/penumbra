//! A projector's colour correction as params: `correct`, `matrix`, `strength`, `gain`, `curves`
//! (group *colour*), parsed into what the output pass applies
//! ([`penumbra_gpu::projection::Correction`]).
//!
//! The matrix works in linear light: `drive = gain * ((1 - strength) I + strength M) * colour`,
//! clamped to 0..1; `strength` likewise blends the curves toward the straight line, so 0 is the
//! projector uncorrected. Each channel's tone curve then maps the drive, sRGB-encoded, to the signal
//! sent to the projector: a table of samples at evenly spaced encoded drives, straight from 0
//! to 1 for a projector that follows the sRGB curve (and for no curve at all).
//! `projector.match` measures both with a camera and writes them.

use std::sync::Arc;

use penumbra_calib::colour::{IDENTITY, Mat3, blend_matrix};
use penumbra_core::Value;
use penumbra_gpu::projection::{Correction, MAX_CURVE};

/// The params a correction is made of.
pub const PARAMS: [&str; 5] = ["correct", "strength", "gain", "matrix", "curves"];

/// Largest magnitude of a matrix entry.
pub const MAX_ENTRY: f64 = 16.0;

/// The matrix in `text`: three rows of three numbers, rows on lines of their own or separated
/// by `/`, numbers by spaces or commas. Empty is the identity.
pub fn parse_matrix(text: &str) -> Result<Mat3, String> {
    let rows: Vec<&str> = text.split(['\n', '/']).map(str::trim).filter(|l| !l.is_empty()).collect();
    if rows.is_empty() {
        return Ok(IDENTITY);
    }
    if rows.len() != 3 {
        return Err(format!(
            "matrix: {} rows; three rows of three numbers, e.g. `1 0 0 / 0 1 0 / 0 0 1`",
            rows.len()
        ));
    }
    let mut m = IDENTITY;
    for (r, (row, out)) in rows.iter().zip(m.iter_mut()).enumerate() {
        let numbers = numbers(row).map_err(|e| format!("matrix row {}: {e}", r + 1))?;
        let [a, b, c] = numbers.as_slice() else {
            return Err(format!("matrix row {} has {} numbers, not 3", r + 1, numbers.len()));
        };
        for v in [a, b, c] {
            if v.abs() > MAX_ENTRY {
                return Err(format!("matrix row {}: `{v}` is beyond ±{MAX_ENTRY}", r + 1));
            }
        }
        *out = [*a, *b, *c];
    }
    Ok(m)
}

/// The text [`parse_matrix`] reads, one row per line, five decimals.
pub fn matrix_text(m: &Mat3) -> String {
    m.iter()
        .map(|row| row.iter().map(|v| number(*v)).collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The tone curves in `text`: one line per channel, `r:`, `g:`, `b:` and the samples (0..1,
/// 2 to [`MAX_CURVE`] of them, spaces or commas), the label optional; or a single line for all
/// three. Empty is none (the straight line).
pub fn parse_curves(text: &str) -> Result<Option<[Vec<f64>; 3]>, String> {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let parse = |line: &str, label: Option<&str>| -> Result<Vec<f64>, String> {
        let (head, body) = match line.split_once(':') {
            Some((h, b)) => (Some(h.trim()), b),
            None => (None, line),
        };
        let name = label.unwrap_or("rgb");
        if let Some(h) = head
            && !h.eq_ignore_ascii_case(name)
        {
            return Err(format!("curves: `{h}:` where `{name}:` belongs (lines are r, g, b in order)"));
        }
        let v = numbers(body).map_err(|e| format!("curves {name}: {e}"))?;
        if !(2..=MAX_CURVE).contains(&v.len()) {
            return Err(format!("curves {name}: {} samples; 2 to {MAX_CURVE}", v.len()));
        }
        if let Some(bad) = v.iter().find(|x| !(0.0..=1.0).contains(*x)) {
            return Err(format!("curves {name}: `{bad}` is outside 0..1"));
        }
        Ok(v)
    };
    match lines.as_slice() {
        [] => Ok(None),
        [one] => {
            let v = parse(one, None)?;
            Ok(Some([v.clone(), v.clone(), v]))
        }
        [r, g, b] => Ok(Some([parse(r, Some("r"))?, parse(g, Some("g"))?, parse(b, Some("b"))?])),
        more => Err(format!("curves: {} lines; one per channel (r, g, b), or one for all", more.len())),
    }
}

/// The text [`parse_curves`] reads, `r:`, `g:`, `b:` lines, five decimals.
pub fn curves_text(curves: &[Vec<f64>; 3]) -> String {
    ["r", "g", "b"]
        .iter()
        .zip(curves)
        .map(|(l, c)| format!("{l}: {}", c.iter().map(|v| number(*v)).collect::<Vec<_>>().join(" ")))
        .collect::<Vec<_>>()
        .join("\n")
}

fn number(v: f64) -> String {
    let s = format!("{v:.5}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" || s.is_empty() { "0".into() } else { s.to_string() }
}

fn numbers(text: &str) -> Result<Vec<f64>, String> {
    text.split([' ', ',', '\t'])
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| match t.parse::<f64>() {
            Ok(v) if v.is_finite() => Ok(v),
            _ => Err(format!("`{t}` is not a number")),
        })
        .collect()
}

/// The correction a projector's params ask for, read through `param`; `None` when it changes
/// nothing (off, or every part the identity), so the output pass skips it.
pub fn correction<'a>(param: impl Fn(&str) -> Option<&'a Value>) -> Result<Option<Arc<Correction>>, String> {
    let text = |name: &str| param(name).and_then(Value::as_str).unwrap_or_default();
    let float = |name: &str| param(name).and_then(Value::as_f64).filter(|v| v.is_finite()).unwrap_or(1.0);
    // Parsed even when off, so a typo shows before it is switched on.
    let matrix = parse_matrix(text("matrix"))?;
    let curves = parse_curves(text("curves"))?;
    if !param("correct").and_then(Value::as_bool).unwrap_or(true) {
        return Ok(None);
    }
    let gain = float("gain").clamp(0.0, 1.0);
    let strength = float("strength").clamp(0.0, 1.0);
    let m = blend_matrix(&matrix, strength).map(|row| row.map(|v| v * gain));
    let curves = curves.map(|c| {
        c.map(|curve| {
            let n = curve.len().saturating_sub(1).max(1) as f64;
            curve
                .iter()
                .enumerate()
                .map(|(i, v)| (1.0 - strength) * i as f64 / n + strength * v)
                .collect::<Vec<f64>>()
        })
    });
    let straight = |c: &[f64]| {
        let n = c.len().saturating_sub(1).max(1) as f64;
        c.iter().enumerate().all(|(i, v)| (v - i as f64 / n).abs() < 1e-6)
    };
    let curves = curves.filter(|c| !c.iter().all(|c| straight(c)));
    let identity = m.iter().flatten().zip(IDENTITY.iter().flatten()).all(|(a, b)| (a - b).abs() < 1e-9);
    if identity && curves.is_none() {
        return Ok(None);
    }
    let curves = curves.unwrap_or_else(|| [vec![0.0, 1.0], vec![0.0, 1.0], vec![0.0, 1.0]]);
    Ok(Some(Arc::new(Correction {
        matrix: m.map(|row| row.map(|v| v as f32)),
        curves: curves.map(|c| c.into_iter().map(|v| v as f32).collect()),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn matrix_text_round_trips_and_refuses_junk() {
        assert_eq!(parse_matrix("").unwrap(), IDENTITY);
        assert_eq!(parse_matrix("  \n ").unwrap(), IDENTITY);
        let m = parse_matrix("0.9 0.1 0 / 0, 0.8, 0.05 / 0 0 0.7").unwrap();
        assert_eq!(m, [[0.9, 0.1, 0.0], [0.0, 0.8, 0.05], [0.0, 0.0, 0.7]]);
        assert_eq!(parse_matrix(&matrix_text(&m)).unwrap(), m);
        assert_eq!(matrix_text(&IDENTITY), "1 0 0\n0 1 0\n0 0 1");
        for bad in [
            "1 0 0",
            "1 0 0 / 0 1 / 0 0 1",
            "1 0 x / 0 1 0 / 0 0 1",
            "NaN 0 0/0 1 0/0 0 1",
            "inf 0 0/0 1 0/0 0 1",
            "99 0 0/0 1 0/0 0 1",
            "1 0 0/0 1 0/0 0 1/1 1 1",
            "1e999 0 0/0 1 0/0 0 1",
        ] {
            assert!(parse_matrix(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn curves_text_round_trips_and_refuses_junk() {
        assert_eq!(parse_curves("").unwrap(), None);
        let one = parse_curves("0 0.5 1").unwrap().unwrap();
        assert_eq!(one[2], vec![0.0, 0.5, 1.0]);
        let three = parse_curves("r: 0 1\ng: 0, 0.4, 1\nB: 0 0.3 0.6 1").unwrap().unwrap();
        assert_eq!(three[1], vec![0.0, 0.4, 1.0]);
        assert_eq!(parse_curves(&curves_text(&three)).unwrap().unwrap(), three);
        let long = vec!["0.5"; MAX_CURVE + 1].join(" ");
        for bad in [
            "0",
            "0 1.5",
            "0 -0.1",
            "0 x",
            "g: 0 1\nr: 0 1\nb: 0 1",
            "0 1\n0 1",
            "0 1\n0 1\n0 1\n0 1",
            "0 NaN",
            long.as_str(),
            "r: 0 1",
        ] {
            assert!(parse_curves(bad).is_err(), "{bad}");
        }
    }

    fn params(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect()
    }

    #[test]
    fn identity_and_off_cost_nothing() {
        let none = |p: BTreeMap<String, Value>| correction(|n| p.get(n)).unwrap().is_none();
        assert!(none(params(&[])));
        assert!(none(params(&[
            ("matrix", Value::Text("1 0 0/0 1 0/0 0 1".into())),
            ("curves", Value::Text("0 0.25 0.5 0.75 1".into()))
        ])));
        let m = Value::Text("0.5 0 0/0 0.5 0/0 0 0.5".into());
        assert!(none(params(&[("matrix", m.clone()), ("correct", Value::Bool(false))])));
        assert!(none(params(&[("matrix", m.clone()), ("strength", Value::Float(0.0))])));
        let bent = Value::Text("0 0.1 1".into());
        assert!(none(params(&[
            ("matrix", m.clone()),
            ("curves", bent.clone()),
            ("strength", Value::Float(0.0))
        ])));
        let p = params(&[("curves", bent), ("strength", Value::Float(0.5))]);
        let c = correction(|n| p.get(n)).unwrap().unwrap();
        assert!((c.curves[1][1] - 0.3).abs() < 1e-6, "{c:?}");
        // A broken text is an error even while off.
        let p = params(&[("matrix", Value::Text("oops".into())), ("correct", Value::Bool(false))]);
        assert!(correction(|n| p.get(n)).is_err());
        // Strength and gain fold into the matrix.
        let p = params(&[("matrix", m), ("strength", Value::Float(0.5)), ("gain", Value::Float(0.8))]);
        let c = correction(|n| p.get(n)).unwrap().unwrap();
        assert!((c.matrix[0][0] - 0.6).abs() < 1e-6 && c.matrix[0][1] == 0.0, "{c:?}");
        assert_eq!(c.curves[0], vec![0.0, 1.0]);
        let p = params(&[("gain", Value::Float(0.5))]);
        assert!((correction(|n| p.get(n)).unwrap().unwrap().matrix[2][2] - 0.5).abs() < 1e-6);
    }
}
