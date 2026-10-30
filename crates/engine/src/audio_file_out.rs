//! Sound recording in the engine: each `out.audio_file` node records through the record hub, as
//! `out.video` does.
//!
//! Commands: `audio_file_out.start`, `audio_file_out.stop`, `audio_file_out.status`. Starting
//! and stopping set the node's `record` param, an edit of its own (undoable) through the gate,
//! with the same waiting and refusals as `video_out.start` (see [`crate::video_out`]).

use std::sync::Arc;
use std::time::{Duration, Instant};

use penumbra_core::{NodeId, NodePath};
use penumbra_ops::out::audio_file::AUDIO_FILE_OUT;
use penumbra_record::SoundStatus;
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, Registry, args};
use crate::commands::engine_commands;
use crate::engine::Engine;
use crate::video_out::{record_param, set_record};

type CmdResult<T> = std::result::Result<T, CommandError>;

/// How long `audio_file_out.start` and `.stop` wait for the node to act on the request. A
/// recording's file appears with its first sound, which may not come (the show paused).
const ANSWER: Duration = Duration::from_secs(1);

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let mut add = engine_commands(registry, engine);
    add(
        "audio_file_out.start",
        "Start recording an out.audio_file node to a new WAV file (its params say what and how): turns its record param on (an edit); {path}; answers with the status once the file is created, or with a note while no sound came yet",
        start,
    );
    add(
        "audio_file_out.stop",
        "Stop an out.audio_file's recording and finish the file: turns record off (an edit); {path}; answers with the status",
        stop,
    );
    add(
        "audio_file_out.status",
        "Sound recordings: node, recording, finishing, done, file, opened, format, rate, channels, seconds, samples, bytes, gap_samples, cut_samples, refused, dropped, sources (out.audio nodes heard with source all, else null), error; {path?}",
        status,
    );
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathArg {
    path: NodePath,
}

/// The `out.audio_file` node at `path`.
fn node_at(e: &Engine, path: &NodePath) -> CmdResult<NodeId> {
    let net = e.session().doc().snapshot();
    let id = net.resolve(path).ok_or_else(|| CommandError(format!("no node {path}")))?;
    let kind = net.node(id).map(|n| n.kind.clone()).unwrap_or_default();
    if kind != AUDIO_FILE_OUT {
        return Err(CommandError(format!("{path} is a {kind}, not an {AUDIO_FILE_OUT}")));
    }
    Ok(id)
}

/// `s` with the node's path instead of its name.
fn with_path(e: &Engine, id: NodeId, mut s: SoundStatus) -> Json {
    if let Some(p) = e.session().doc().snapshot().path_of(id) {
        s.node = p.to_string();
    }
    serde_json::to_value(s).unwrap_or(Json::Null)
}

fn switch(e: &Engine, p: &Json, on: bool) -> CmdResult<Json> {
    let a: PathArg = args(p)?;
    let id = node_at(e, &a.path)?;
    let hub = e.record();
    let (stored, bound) = record_param(e, id);
    if bound {
        return Err(CommandError(format!(
            "{}: `record` follows a channel (a binding); unbind it to start and stop by command",
            a.path
        )));
    }
    let before = hub.sound_status(id);
    let recording = before.as_ref().is_some_and(|s| s.recording);
    if stored == on {
        if on && !recording {
            let why = before.and_then(|s| s.error).unwrap_or_else(|| "see node.info".into());
            return Err(CommandError(format!(
                "{}: `record` is on but it is not recording ({why}); audio_file_out.stop, then start again",
                a.path
            )));
        }
    } else {
        set_record(e, &a.path, on)?;
    }
    // The node acts at its next cook: wait until its status moves, and for a new recording
    // until its file is created, so the answer names the file actually written.
    let already = stored && recording;
    let deadline = Instant::now() + ANSWER;
    loop {
        let now = hub.sound_status(id);
        let settled = if on {
            now.as_ref().is_some_and(|s| {
                let ended = s.error.is_some() || s.done;
                (already || now != before) && (ended || (s.recording && s.opened))
            })
        } else {
            now.as_ref().is_none_or(|s| !s.recording)
        };
        let late = Instant::now() >= deadline;
        if settled || late {
            let mut out = match &now {
                Some(s) => with_path(e, id, s.clone()),
                None => json!({ "node": a.path.to_string(), "recording": false }),
            };
            if late
                && !settled
                && let Some(o) = out.as_object_mut()
            {
                let note = if on && now.as_ref().is_some_and(|s| s.recording && !s.opened) {
                    "recording, no sound yet: the file is created with the first sound (is the show paused?)"
                } else {
                    "the node has not acted yet; node.info shows its error, if any"
                };
                o.insert("note".into(), json!(note));
            }
            return Ok(out);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn start(e: &Engine, p: &Json) -> CmdResult<Json> {
    switch(e, p, true)
}

fn stop(e: &Engine, p: &Json) -> CmdResult<Json> {
    switch(e, p, false)
}

fn status(e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        path: Option<NodePath>,
    }
    let a: A = args(p)?;
    let hub = e.record();
    if let Some(path) = a.path {
        let id = node_at(e, &path)?;
        return Ok(match hub.sound_status(id) {
            Some(s) => with_path(e, id, s),
            None => json!({ "node": path.to_string(), "recording": false }),
        });
    }
    let list: Vec<Json> = hub.all_sound().into_iter().map(|(id, s)| with_path(e, id, s)).collect();
    Ok(json!({ "recordings": list }))
}
