//! `penumbra-cli`: penumbra on the command line.
//!
//! ```text
//! penumbra-cli ctl <method> [params]   send one request to a running app, print the reply
//! penumbra-cli [--port N] ...          control channel port (default: PENUMBRA_PORT or 9880)
//! penumbra-cli [--timeout S] ...       longest wait for a reply in seconds (default 300; some
//!                                      commands, like projector.match, take a minute)
//! penumbra-cli [--port N] mcp          serve MCP on stdin/stdout for a running app (Claude Code:
//!                                      `claude mcp add penumbra -- penumbra-cli mcp`)
//! penumbra-cli soak [options]          run the engine headless for a while, report leaks and stalls
//! ```
//!
//! `params` is either one JSON object (`'{"a": 1}'`) or `key=value` pairs, where each value is
//! parsed as JSON if it can be and taken as a string otherwise (`name=noise1 amp=0.5`).
//! Without arguments, `penumbra-cli` pings the app. Started by a double-click, which gives it a
//! console window of its own that closes when it exits, it also says what it is and waits for
//! Enter, so the window does not just flash and vanish.
//!
//! Exit code: 0 when the reply has `"ok": true`, 1 otherwise.

mod heap;
mod mcp;
mod soak;

/// Counts the Rust heap for the soak test (`heap.rs`).
#[global_allocator]
static ALLOC: heap::Counting = heap::Counting;

use std::process::ExitCode;
use std::time::Duration;

use penumbra_engine::control;
use serde_json::{Map, Value};

const USAGE: &str = "usage: penumbra-cli [--port N] [--timeout S] ctl <method> [{json} | key=value ...]
       penumbra-cli [--port N] [--timeout S] mcp
       penumbra-cli soak --help";
/// How long a reply may take unless `--timeout` says otherwise: commands that measure (a camera
/// looking at every projector) legitimately run for a minute or more.
const TIMEOUT: Duration = Duration::from_secs(300);

/// Shown when the tool is started by a double-click instead of from a terminal.
const DOUBLE_CLICKED: &str = "This is penumbra-cli, the command-line tool of penumbra. It sends commands to a
running penumbra.exe and is meant to be used from a terminal (cmd or PowerShell).
To use penumbra itself, start penumbra.exe instead.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let double_clicked = args.is_empty() && penumbra_system::process::own_console();
    if double_clicked {
        println!("{DOUBLE_CLICKED}");
    }
    let code = match run(args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("penumbra-cli: {e}");
            ExitCode::FAILURE
        }
    };
    if double_clicked {
        println!("\nPress Enter to close this window.");
        let _ = std::io::stdin().read_line(&mut String::new());
    }
    code
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
    let mut timeout = TIMEOUT;
    let mut rest = args.as_slice();
    while let [flag, value, tail @ ..] = rest {
        match flag.as_str() {
            "--port" => port = Some(control::parse_port(value)?),
            "--timeout" => timeout = parse_timeout(value)?,
            _ => break,
        }
        rest = tail;
    }
    let port = match port {
        Some(p) => p,
        None => control::port_from_env()?,
    };
    match rest {
        [] => {
            eprintln!("{USAGE}\nno command given, pinging the app");
            ctl(port, "ping", &[], timeout)
        }
        [cmd, method, params @ ..] if cmd == "ctl" => ctl(port, method, params, timeout),
        [cmd] if cmd == "mcp" => mcp::run(port, timeout),
        _ => Err(USAGE.into()),
    }
}

/// Seconds, more than 0, at most a day.
fn parse_timeout(text: &str) -> Result<Duration, String> {
    match text.trim().parse::<f64>() {
        Ok(s) if s > 0.0 && s <= 86_400.0 => Ok(Duration::from_secs_f64(s)),
        _ => Err(format!("--timeout: `{text}` is not a number of seconds (0 to 86400)")),
    }
}

fn ctl(port: u16, method: &str, params: &[String], timeout: Duration) -> Result<ExitCode, String> {
    let params = parse_params(params)?;
    let reply = control::call(port, method, params, timeout)?;
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

    #[test]
    fn timeouts_parse() {
        assert_eq!(parse_timeout("2.5").unwrap(), Duration::from_millis(2500));
        for bad in ["0", "-1", "x", "NaN", "inf", "1e9"] {
            assert!(parse_timeout(bad).is_err(), "{bad}");
        }
    }

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
