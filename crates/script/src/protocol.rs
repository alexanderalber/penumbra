//! The JSON exchanged with the bootstrap inside Python (`crates/pyhost/src/bootstrap.py`), and
//! its conversion from and to penumbra's types. Pure Rust, tested without a Python.

use std::collections::BTreeMap;
use std::path::Path;

use penumbra_core::{Channel, Chans, Name, ParamSpec, Table, Value};
use serde::{Deserialize, Serialize};

/// Loading a script into the module slot `key` (replacing what was there only if it loads).
#[derive(Clone, Debug)]
pub struct Load<'a> {
    pub key: u64,
    /// Increases with every load of a slot: a load older than the one in the slot is refused,
    /// so two loads racing each other cannot leave the older script running.
    pub generation: u64,
    pub source: &'a str,
    /// The name errors and tracebacks use (the file's name, or the node's).
    pub label: &'a str,
    /// Put on `sys.path`, so a script can import modules next to it.
    pub folder: Option<&'a Path>,
}

/// What a script declared when it loaded.
#[derive(Clone, Debug, PartialEq)]
pub struct Loaded {
    /// From its `PARAMS`.
    pub params: Vec<ParamSpec>,
    /// Names `time` or `frame` somewhere: recook every frame.
    pub uses_time: bool,
    pub printed: String,
}

/// One call of a loaded script's `cook(cx)`.
#[derive(Clone, Debug)]
pub struct Call<'a> {
    pub key: u64,
    /// Show time in seconds and frame number; `None` for a script that does not read them.
    pub time: Option<(f64, i64)>,
    /// Show frames per second, also the default rate of the output channels.
    pub rate: f64,
    pub params: &'a BTreeMap<String, Value>,
    pub chans: Vec<Option<&'a Chans>>,
    pub tables: Vec<Option<&'a Table>>,
}

/// What a call produced.
#[derive(Clone, Debug, PartialEq)]
pub struct CallOutput {
    pub chans: Chans,
    pub table: Table,
    pub warnings: Vec<String>,
    pub printed: String,
}

#[derive(Serialize)]
struct LoadJson<'a> {
    op: &'static str,
    key: u64,
    generation: u64,
    source: &'a str,
    file: &'a str,
    folder: Option<String>,
}

#[derive(Serialize)]
struct CallJson<'a> {
    op: &'static str,
    key: u64,
    time: Option<f64>,
    frame: Option<i64>,
    rate: f64,
    params: &'a BTreeMap<String, Value>,
    chans: Vec<Option<ChansJson<'a>>>,
    tables: Vec<Option<&'a [Vec<String>]>>,
}

#[derive(Serialize)]
struct ChansJson<'a> {
    rate: f64,
    names: Vec<&'a str>,
    samples: Vec<&'a [f64]>,
}

/// Every reply has `ok`; the rest depends on the request.
#[derive(Deserialize, Default)]
#[serde(default)]
pub(crate) struct Reply {
    pub ok: bool,
    pub error: String,
    pub timeout: bool,
    pub printed: String,
    // load
    params: Vec<ParamJson>,
    uses_time: bool,
    // call
    names: Vec<String>,
    samples: Vec<Vec<f64>>,
    rate: Option<f64>,
    table: Vec<Vec<String>>,
    warnings: Vec<String>,
}

#[derive(Deserialize)]
struct ParamJson {
    name: String,
    kind: String,
    default: serde_json::Value,
    min: Option<f64>,
    max: Option<f64>,
    #[serde(default)]
    choices: Vec<String>,
}

pub(crate) fn load_json(load: &Load<'_>) -> Result<String, String> {
    serde_json::to_string(&LoadJson {
        op: "load",
        key: load.key,
        generation: load.generation,
        source: load.source,
        file: load.label,
        folder: load.folder.map(|f| f.display().to_string()),
    })
    .map_err(|e| e.to_string())
}

pub(crate) fn call_json(call: &Call<'_>) -> Result<String, String> {
    let chans = call
        .chans
        .iter()
        .map(|c| {
            c.map(|c| ChansJson {
                rate: c.rate(),
                names: c.channels().iter().map(|ch| ch.name.as_str()).collect(),
                samples: c.channels().iter().map(|ch| ch.samples.as_slice()).collect(),
            })
        })
        .collect();
    serde_json::to_string(&CallJson {
        op: "call",
        key: call.key,
        time: call.time.map(|t| t.0),
        frame: call.time.map(|t| t.1),
        rate: call.rate,
        params: call.params,
        chans,
        tables: call.tables.iter().map(|t| t.map(Table::rows)).collect(),
    })
    .map_err(|e| e.to_string())
}

pub(crate) fn parse(text: &str) -> Result<Reply, String> {
    serde_json::from_str(text).map_err(|e| format!("unreadable reply from Python: {e}"))
}

impl Reply {
    pub(crate) fn loaded(self) -> Result<Loaded, String> {
        let params = self.params.into_iter().map(param_spec).collect::<Result<_, _>>()?;
        Ok(Loaded { params, uses_time: self.uses_time, printed: self.printed })
    }

    pub(crate) fn output(self, show_rate: f64) -> Result<CallOutput, String> {
        let channels = self
            .names
            .into_iter()
            .zip(self.samples)
            .map(|(name, samples)| {
                let name = Name::new(&name).map_err(|e| format!("cx.out[{name:?}]: {e}"))?;
                Ok(Channel { name, samples })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let chans =
            Chans::new(self.rate.unwrap_or(show_rate), channels).map_err(|e| format!("cx.out: {e}"))?;
        let table = Table::new(self.table).map_err(|e| format!("cx.table: {e}"))?;
        Ok(CallOutput { chans, table, warnings: self.warnings, printed: self.printed })
    }
}

fn param_spec(p: ParamJson) -> Result<ParamSpec, String> {
    let ctx = |e: String| format!("PARAMS[{:?}]: {e}", p.name);
    let range = |spec: ParamSpec| match (p.min, p.max) {
        (Some(min), Some(max)) => spec.range(min, max),
        _ => spec,
    };
    let spec = match (p.kind.as_str(), &p.default) {
        ("bool", serde_json::Value::Bool(b)) => ParamSpec::bool(&p.name, *b),
        ("int", d) if d.as_i64().is_some() => range(ParamSpec::int(&p.name, d.as_i64().unwrap_or(0))),
        ("float", d) if d.as_f64().is_some() => range(ParamSpec::float(&p.name, d.as_f64().unwrap_or(0.0))),
        ("text", serde_json::Value::String(s)) => ParamSpec::text(&p.name, s),
        ("choice", _) => {
            let choices: Vec<&str> = p.choices.iter().map(String::as_str).collect();
            ParamSpec::choice(&p.name, &choices)
        }
        (kind, d) => return Err(ctx(format!("cannot declare a {kind} with default {d}"))),
    };
    spec.validate_spec().map_err(|e| ctx(e.0))?;
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_json_carries_inputs() {
        let chans =
            Chans::new(30.0, vec![Channel { name: Name::new("x").unwrap(), samples: vec![1.0, 2.5] }])
                .unwrap();
        let table = Table::new(vec![vec!["a".into(), "b".into()]]).unwrap();
        let params = BTreeMap::from([("gain".to_string(), Value::Float(0.5))]);
        let text = call_json(&Call {
            key: 7,
            time: Some((1.5, 90)),
            rate: 60.0,
            params: &params,
            chans: vec![Some(&chans), None],
            tables: vec![None, Some(&table)],
        })
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["op"], "call");
        assert_eq!(v["chans"][0]["names"][0], "x");
        assert_eq!(v["chans"][0]["samples"][0][1], 2.5);
        assert!(v["chans"][1].is_null());
        assert_eq!(v["tables"][1][0][1], "b");
        assert_eq!(v["params"]["gain"], 0.5);
        assert_eq!((v["time"].as_f64(), v["frame"].as_i64()), (Some(1.5), Some(90)));
    }

    #[test]
    fn replies_become_chans_tables_and_params() {
        let r = parse(
            r#"{"ok": true, "names": ["x", "y"], "samples": [[1, 2], [3, 4]], "rate": null,
            "table": [["a"], []], "warnings": ["w"], "printed": "hi\n"}"#,
        )
        .unwrap();
        let out = r.output(60.0).unwrap();
        assert_eq!(out.chans.rate(), 60.0);
        assert_eq!(out.chans.value(Some("y")), Some(4.0));
        assert_eq!(out.table.num_rows(), 2);
        assert_eq!(out.warnings, ["w"]);

        let bad = parse(r#"{"ok": true, "names": ["no space"], "samples": [[1]]}"#).unwrap();
        assert!(bad.output(60.0).unwrap_err().starts_with("cx.out[\"no space\"]"));

        let r = parse(
            r#"{"ok": true, "uses_time": true, "params": [
            {"name": "gain", "kind": "float", "default": 1.0, "min": 0.0, "max": 2.0},
            {"name": "count", "kind": "int", "default": 3},
            {"name": "on", "kind": "bool", "default": false},
            {"name": "label", "kind": "text", "default": "hi"},
            {"name": "mode", "kind": "choice", "default": "a", "choices": ["a", "b"]}]}"#,
        )
        .unwrap();
        let l = r.loaded().unwrap();
        assert!(l.uses_time);
        assert_eq!(l.params.len(), 5);
        assert_eq!(l.params[0].default, Value::Float(1.0));
        let r =
            parse(r#"{"ok": true, "params": [{"name": "bad name", "kind": "int", "default": 1}]}"#).unwrap();
        assert!(r.loaded().unwrap_err().contains("PARAMS[\"bad name\"]"));
    }
}
