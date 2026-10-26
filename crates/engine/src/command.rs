//! The command registry.

use std::collections::BTreeMap;
use std::fmt;

use serde_json::{Value, json};

/// Why a command failed. The message goes back to the caller verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandError(pub String);

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CommandError {}

impl From<penumbra_core::Error> for CommandError {
    fn from(e: penumbra_core::Error) -> Self {
        Self(e.0)
    }
}

impl From<String> for CommandError {
    fn from(message: String) -> Self {
        Self(message)
    }
}

impl From<&str> for CommandError {
    fn from(message: &str) -> Self {
        Self(message.to_string())
    }
}

/// One thing the app can do, reachable by its id from the UI, the CLI and the control channel.
pub trait Command: Send + Sync {
    /// Dotted id, e.g. `node.set`. This is the control channel's `method`.
    fn id(&self) -> &'static str;
    /// One line for listings.
    fn summary(&self) -> &'static str;
    /// Run with the request's `params` (a JSON object, `{}` when the caller sent none).
    fn run(&self, params: &Value) -> Result<Value, CommandError>;
}

/// All commands of a process, by id.
/// Asked before every command: an error message refuses it ([`Registry::set_guard`]).
type Guard = dyn Fn(&str) -> Option<String> + Send + Sync;

#[derive(Default)]
pub struct Registry {
    commands: BTreeMap<&'static str, Box<dyn Command>>,
    guard: Option<Box<Guard>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a command. A later command with the same id replaces the earlier one.
    pub fn register(&mut self, command: impl Command + 'static) {
        self.commands.insert(command.id(), Box::new(command));
    }

    /// Sets the check every command passes before it runs; a message refuses the command with
    /// it (show mode refuses commands that would change the project).
    pub fn set_guard(&mut self, guard: impl Fn(&str) -> Option<String> + Send + Sync + 'static) {
        self.guard = Some(Box::new(guard));
    }

    pub fn ids(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.commands.keys().copied()
    }

    /// Every command with its summary, sorted by id. The control channel answers `commands`
    /// with this list.
    pub fn list(&self) -> Value {
        let list: Vec<Value> =
            self.commands.values().map(|c| json!({"id": c.id(), "summary": c.summary()})).collect();
        json!({"commands": list})
    }

    /// Run `method` and wrap the outcome in the reply envelope:
    /// `{"ok": true, "result": ...}` or `{"ok": false, "error": "..."}`.
    pub fn dispatch(&self, method: &str, params: &Value) -> Value {
        if method == "commands" {
            return json!({"ok": true, "result": self.list()});
        }
        let Some(command) = self.commands.get(method) else {
            return json!({
                "ok": false,
                "error": format!("unknown method `{method}`; `commands` lists them all"),
            });
        };
        if !params.is_object() {
            return json!({"ok": false, "error": "`params` must be a JSON object"});
        }
        if let Some(refused) = self.guard.as_ref().and_then(|g| g(method)) {
            return json!({"ok": false, "error": refused});
        }
        match command.run(params) {
            Ok(result) => json!({"ok": true, "result": result}),
            Err(e) => json!({"ok": false, "error": e.0}),
        }
    }
}

type RunFn = dyn Fn(&Value) -> Result<Value, CommandError> + Send + Sync;

/// A command made from an id, a summary and a closure.
pub struct FnCommand {
    id: &'static str,
    summary: &'static str,
    run: Box<RunFn>,
}

impl FnCommand {
    pub fn new(
        id: &'static str,
        summary: &'static str,
        run: impl Fn(&Value) -> Result<Value, CommandError> + Send + Sync + 'static,
    ) -> Self {
        Self { id, summary, run: Box::new(run) }
    }
}

impl Command for FnCommand {
    fn id(&self) -> &'static str {
        self.id
    }

    fn summary(&self) -> &'static str {
        self.summary
    }

    fn run(&self, params: &Value) -> Result<Value, CommandError> {
        (self.run)(params)
    }
}

/// `params` as the typed argument struct of a command. Unknown fields should be refused by
/// the struct (`deny_unknown_fields`), so a misspelt parameter is an error, not ignored.
pub fn args<T: serde::de::DeserializeOwned>(params: &Value) -> Result<T, CommandError> {
    serde_json::from_value(params.clone()).map_err(|e| CommandError(format!("bad params: {e}")))
}

/// `ping`: proves the channel is alive and says who answers.
pub struct Ping;

impl Command for Ping {
    fn id(&self) -> &'static str {
        "ping"
    }

    fn summary(&self) -> &'static str {
        "Check that the control channel answers"
    }

    fn run(&self, _params: &Value) -> Result<Value, CommandError> {
        Ok(json!({"pong": true, "app": "penumbra", "version": crate::VERSION}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_answers() {
        let r = crate::builtin_registry().dispatch("ping", &json!({}));
        assert_eq!(r["ok"], true);
        assert_eq!(r["result"]["pong"], true);
        assert_eq!(r["result"]["version"], crate::VERSION);
    }

    #[test]
    fn unknown_method_is_an_error_not_a_crash() {
        let r = crate::builtin_registry().dispatch("nope", &json!({}));
        assert_eq!(r["ok"], false);
        assert!(r["error"].as_str().unwrap().contains("nope"));
    }

    #[test]
    fn commands_lists_the_registry() {
        let r = crate::builtin_registry().dispatch("commands", &json!({}));
        assert_eq!(r["result"]["commands"][0]["id"], "ping");
    }

    #[test]
    fn params_must_be_an_object() {
        let r = crate::builtin_registry().dispatch("ping", &json!([1, 2]));
        assert_eq!(r["ok"], false);
    }
}
