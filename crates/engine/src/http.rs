//! HTTP and WebSocket in the engine: the hub's listeners follow the network's `chan.http_in`
//! nodes ([`penumbra_ops::http::wants`]), their messages go to the OSC hub, so the osc thread
//! sets params by address and learn binds as for OSC. The state sent to a page lists the show
//! screen's controls ([`controls`]), so a page can show and move them.
//!
//! Commands: `http.status`, `http.config`, `http.send`.

use std::sync::{Arc, Weak};

use penumbra_core::Value;
use penumbra_graph::NodeDef;
use penumbra_http::hub::{Config, ListenerStatus};
use penumbra_ops::chan::controls::SLIDER;
use penumbra_ops::http::HTTP_IN;
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::command::{CommandError, Registry, args};
use crate::commands::{engine_commands, no_args};
use crate::engine::Engine;

type CmdResult<T> = std::result::Result<T, CommandError>;

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let weak: Weak<Engine> = Arc::downgrade(engine);
    engine
        .http()
        .set_controls(Arc::new(move || weak.upgrade().map_or(Json::Array(Vec::new()), |e| controls(&e))));
    let mut add = engine_commands(registry, engine);
    add(
        "http.status",
        "HTTP listeners of chan.http_in nodes: port, bound, the page's address for a phone, connections, \
         WebSocket clients, messages per second, the last addresses, drops by cause",
        http_status,
    );
    add(
        "http.config",
        "HTTP settings for this run: {enabled?, max_body?, rate?, burst?, max_connections?, max_per_client?, \
         max_addresses?, timeout?, idle?}",
        http_config,
    );
    add(
        "http.send",
        "Take in a message on a chan.http_in's port as if a page had sent it: {message: {\"/fader\": 0.5}, \
         port?} (port as the node's `port` param; may be left out when one listens)",
        http_send,
    );
}

/// The show screen's controls as a page gets them: what `show.status` lists, plus the address
/// that sets each one's value and a slider's ends.
pub fn controls(e: &Engine) -> Json {
    let net = e.session().doc().snapshot();
    let types = e.types();
    let list = crate::buttons::show_controls(&net, types, e.buttons(), Some(&e.view().status));
    let out = list
        .into_iter()
        .map(|c| {
            let node = net.node(c.id);
            let mut v = serde_json::to_value(&c).unwrap_or(Json::Null);
            if let Some(o) = v.as_object_mut() {
                o.insert("address".into(), json!(format!("{}/value", c.path)));
                if c.kind == SLIDER
                    && let Some(n) = node
                {
                    o.insert("min".into(), json!(param_f64(e, n, "min", 0.0)));
                    o.insert("max".into(), json!(param_f64(e, n, "max", 1.0)));
                }
            }
            v
        })
        .collect();
    Json::Array(out)
}

fn param_f64(e: &Engine, node: &NodeDef, name: &str, or: f64) -> f64 {
    e.types().desc(&node.kind).and_then(|d| node.param(d, name)).and_then(Value::as_f64).unwrap_or(or)
}

/// The port number in a bound address (`127.0.0.1:8090`).
fn bound_port(s: &ListenerStatus) -> Option<u16> {
    s.bound.as_deref()?.rsplit(':').next()?.parse().ok()
}

/// Where a phone (or this machine, for a listener on localhost) opens the page.
fn page(s: &ListenerStatus) -> Option<String> {
    let port = bound_port(s)?;
    let host = if s.lan {
        penumbra_osc::hub::local_ip().map_or_else(|| "<this machine>".to_string(), |ip| ip.to_string())
    } else {
        "localhost".to_string()
    };
    Some(format!("http://{host}:{port}/"))
}

fn listener_json(s: &ListenerStatus) -> Json {
    let mut v = serde_json::to_value(s).unwrap_or(Json::Null);
    if let Some(o) = v.as_object_mut() {
        o.insert("page".into(), json!(page(s)));
    }
    v
}

/// What `node.info` adds for a `chan.http_in`: its listener's status (clients, messages per
/// second, last addresses) and the page's address.
pub fn info(e: &Engine, node: &NodeDef) -> Option<Json> {
    if node.kind != HTTP_IN {
        return None;
    }
    let port = e.types().desc(HTTP_IN).and_then(|d| node.param(d, "port")).and_then(Value::as_i64)?;
    let port = u16::try_from(port).ok()?;
    Some(match e.http().listener(port) {
        Some(s) => listener_json(&s),
        None => json!({"port": port, "listening": false}),
    })
}

fn http_status(e: &Engine, p: &Json) -> CmdResult<Json> {
    no_args(p)?;
    let hub = e.http();
    let listeners: Vec<Json> = hub.status().iter().map(listener_json).collect();
    Ok(json!({
        "enabled": hub.config().enabled,
        "listeners": listeners,
        "local_ip": penumbra_osc::hub::local_ip().map(|ip| ip.to_string()),
        "config": hub.config(),
    }))
}

fn http_config(e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        enabled: Option<bool>,
        max_body: Option<usize>,
        rate: Option<f64>,
        burst: Option<f64>,
        max_connections: Option<usize>,
        max_per_client: Option<usize>,
        max_addresses: Option<usize>,
        timeout: Option<f64>,
        idle: Option<f64>,
    }
    let a: A = args(p)?;
    let hub = e.http();
    let mut c: Config = hub.config();
    if let Some(v) = a.enabled {
        c.enabled = v;
    }
    if let Some(v) = a.max_body {
        c.max_body = v;
    }
    if let Some(v) = a.rate {
        c.rate = v;
    }
    if let Some(v) = a.burst {
        c.burst = v;
    }
    if let Some(v) = a.max_connections {
        c.max_connections = v;
    }
    if let Some(v) = a.max_per_client {
        c.max_per_client = v;
    }
    if let Some(v) = a.max_addresses {
        c.max_addresses = v;
    }
    if let Some(v) = a.timeout {
        c.timeout = v;
    }
    if let Some(v) = a.idle {
        c.idle = v;
    }
    hub.set_config(c);
    Ok(serde_json::to_value(hub.config()).map_err(|e| e.to_string())?)
}

fn http_send(e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        message: Json,
        port: Option<u16>,
    }
    let a: A = args(p)?;
    let hub = e.http();
    let port = match a.port {
        Some(p) => p,
        None => {
            let wanted = hub.wanted();
            match wanted.as_slice() {
                [one] => one.port,
                [] => return Err("no chan.http_in listens".into()),
                _ => return Err("several chan.http_in ports listen; say which with `port`".into()),
            }
        }
    };
    let addresses = hub.inject(port, &a.message.to_string())?;
    Ok(json!({"port": port, "addresses": addresses}))
}
