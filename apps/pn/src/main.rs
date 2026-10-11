//! `pn`: penumbra on the command line.
//!
//! ```text
//! pn ctl <method> [params]     send one request to a running app, print the reply
//! pn [--port N] ...            control channel port (default: PENUMBRA_PORT or 9880)
//! pn soak [options]            run the engine headless for a while, report leaks and stalls
//! ```
//!
//! `params` is either one JSON object (`'{"a": 1}'`) or `key=value` pairs, where each value is
//! parsed as JSON if it can be and taken as a string otherwise (`name=noise1 amp=0.5`).
//! Without arguments, `pn` pings the app.
//!
//! Exit code: 0 when the reply has `"ok": true`, 1 otherwise.

mod heap;
mod soak;

/// Counts the Rust heap for the soak test (`heap.rs`).
#[global_allocator]
static ALLOC: heap::Counting = heap::Counting;

use std::process::ExitCode;
use std::time::Duration;

use penumbra_engine::control;
use serde_json::{Map, Value};

const USAGE: &str = "usage: pn [--port N] ctl <method> [{json} | key=value ...]
       pn soak --help";
const TIMEOUT: Duration = Duration::from_secs(30);

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("pn: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> Result<ExitCode, String> {
    if let [cmd, rest @ ..] = args.as_slice()
        && cmd == "soak"
    {
        if rest.iter().any(|a| a == "--help" || a == "-h") {
            println!("usage: {}", soak::USAGE);
            return Ok(ExitCode::SUCCESS);
        }
        return soak::run(rest);
    }
    let mut port = None;
    let mut rest = args.as_slice();
    if let [flag, value, tail @ ..] = rest
        && flag == "--port"
    {
        port = Some(control::parse_port(value)?);
        rest = tail;
    }
    let port = match port {
        Some(p) => p,
        None => control::port_from_env()?,
    };
    match rest {
        [] => {
            eprintln!("{USAGE}\nno command given, pinging the app");
            ctl(port, "ping", &[])
        }
        [cmd, method, params @ ..] if cmd == "ctl" => ctl(port, method, params),
        _ => Err(USAGE.into()),
    }
}

fn ctl(port: u16, method: &str, params: &[String]) -> Result<ExitCode, String> {
    let params = parse_params(params)?;
    let reply = control::call(port, method, params, TIMEOUT)?;
    let text = serde_json::to_string_pretty(&reply).map_err(|e| e.to_string())?;
    println!("{text}");
    Ok(if reply.get("ok") == Some(&Value::Bool(true)) { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

fn parse_params(args: &[String]) -> Result<Value, String> {
    if let [only] = args
        && only.trim_start().starts_with('{')
    {
        return match serde_json::from_str::<Value>(only) {
            Ok(v @ Value::Object(_)) => Ok(v),
            Ok(_) => Err("params must be a JSON object".into()),
            Err(e) => Err(format!("params are not valid JSON: {e}")),
        };
    }
    let mut map = Map::new();
    for arg in args {
        let (key, value) = arg
            .split_once('=')
            .ok_or_else(|| format!("expected key=value or one JSON object, got `{arg}`"))?;
        if key.is_empty() {
            return Err(format!("empty key in `{arg}`"));
        }
        let value = serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.to_string()));
        map.insert(key.to_string(), value);
    }
    Ok(Value::Object(map))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn params_from_json_or_pairs() {
        assert_eq!(parse_params(&[]).unwrap(), json!({}));
        assert_eq!(parse_params(&strings(&[r#"{"a": [1, 2]}"#])).unwrap(), json!({"a": [1, 2]}));
        assert_eq!(
            parse_params(&strings(&["name=noise1", "amp=0.5", "on=true", "path=a=b"])).unwrap(),
            json!({"name": "noise1", "amp": 0.5, "on": true, "path": "a=b"})
        );
        assert!(parse_params(&strings(&["{broken"])).is_err());
        assert!(parse_params(&strings(&["novalue"])).is_err());
        assert!(parse_params(&strings(&["=1"])).is_err());
    }
}
