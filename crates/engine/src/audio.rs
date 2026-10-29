//! Audio in the engine: the hub's streams follow the network's `audio.device_in` nodes (the cook
//! thread calls [`penumbra_ops::audio::wants`] when the network changes), the output buses the
//! `out.audio` nodes ([`penumbra_ops::out::audio::wants`]).
//!
//! Commands: `audio.devices`, `audio.outputs`, `audio.status`, `audio.config`, `audio.mute`,
//! `audio.measure_latency`.

use std::sync::Arc;

use penumbra_audio::capture::{self, Config};
use penumbra_audio::measure;
use penumbra_audio::playback::{self, Settings};
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, Registry, args};
use crate::commands::engine_commands;
use crate::engine::Engine;

type CmdResult<T> = std::result::Result<T, CommandError>;

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let mut add = engine_commands(registry, engine);
    add(
        "audio.devices",
        "The audio inputs of this machine: name, default, rate, channels, format",
        audio_devices,
    );
    add(
        "audio.outputs",
        "The audio outputs of this machine: name, default, rate, channels, format",
        audio_outputs,
    );
    add(
        "audio.status",
        "Audio input streams (device, rate, channels, open, error, frames, glitches, peak per channel in dBFS) and          output buses (device, open, error, rate, latency_ms with fill_ms and device_ms, underruns, overruns,          glitches, limiter reduction per node and of the sum, muted)",
        audio_status,
    );
    add("audio.config", "Audio settings for this run: {enabled?}", audio_config);
    add(
        "audio.mute",
        "Mutes every audio output at once, a panic button: {mute?} (default true; false unmutes). For this run only",
        audio_mute,
    );
    add(
        "audio.measure_latency",
        "Plays quiet clicks (at most -20 dBFS) on an output and finds them on an input: {output?, input?,          channels?, clicks?, level?, latency?, buffer?}; input `loopback:<output>` records what the output plays.          Reports ms from push to arrival per channel, and an output estimate. Takes some seconds",
        audio_measure_latency,
    );
}

fn audio_outputs(_e: &Engine, _p: &Json) -> CmdResult<Json> {
    let list = playback::output_devices().map_err(CommandError)?;
    Ok(json!({ "devices": list }))
}

fn audio_mute(e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        mute: Option<bool>,
    }
    let a: A = args(p)?;
    let out = e.audio_out();
    out.set_muted(a.mute.unwrap_or(true));
    Ok(json!({ "muted": out.muted() }))
}

fn audio_measure_latency(_e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        output: Option<String>,
        input: Option<String>,
        channels: Option<Vec<usize>>,
        clicks: Option<usize>,
        level: Option<f64>,
        latency: Option<f64>,
        buffer: Option<f64>,
    }
    let a: A = args(p)?;
    let defaults = measure::Spec::default();
    let settings = Settings {
        latency_ms: a.latency.unwrap_or(Settings::default().latency_ms),
        device_buffer_ms: a.buffer.unwrap_or(0.0),
    };
    let spec = measure::Spec {
        output: a.output.unwrap_or_default(),
        input: a.input.unwrap_or_default(),
        channels: a.channels.unwrap_or_default(),
        clicks: a.clicks.unwrap_or(defaults.clicks),
        level_db: a.level.unwrap_or(defaults.level_db).min(measure::MAX_LEVEL_DB),
        settings,
    };
    let report = measure::run(&spec).map_err(CommandError)?;
    Ok(serde_json::to_value(report).map_err(|e| e.to_string())?)
}

fn audio_devices(_e: &Engine, _p: &Json) -> CmdResult<Json> {
    let list = capture::devices().map_err(CommandError)?;
    Ok(json!({ "devices": list }))
}

fn audio_status(e: &Engine, _p: &Json) -> CmdResult<Json> {
    let hub = e.audio();
    let out = e.audio_out();
    Ok(json!({
        "enabled": hub.config().enabled,
        "wanted": hub.wanted(),
        "streams": hub.status(),
        "muted": out.muted(),
        "outputs": out.status(),
    }))
}

fn audio_config(e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        enabled: Option<bool>,
    }
    let a: A = args(p)?;
    let hub = e.audio();
    let mut c: Config = hub.config();
    if let Some(v) = a.enabled {
        c.enabled = v;
    }
    hub.set_config(c);
    Ok(serde_json::to_value(hub.config()).map_err(|e| e.to_string())?)
}
