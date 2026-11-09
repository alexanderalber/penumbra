//! `chan.http_in`: HTTP and WebSocket in, for a control page in the browser (a phone on the
//! LAN) and for other programs.
//!
//! It listens on a TCP port (this machine only unless `bind` is `lan`) and makes every address
//! that arrives a channel holding its latest number, as `chan.osc_in` does: `{"/fader": 0.5}`
//! becomes channel `fader`. A table sent to an address (`{"/cues": [["a", 1]]}`) becomes the
//! `table` output. With `params`, the same messages also set params by address (the engine
//! does that, through the OSC hub). The node serves the files of a folder in the project, so a
//! page there opens on a phone without any other server, and it sends its input's channels
//! back to the page's WebSocket. Without a page in that folder, `/` opens a built-in one that
//! draws the show screen's controls (`page` switches it off). The node recooks when something
//! arrives.
//!
//! The listener belongs to the engine's [`Hub`], which opens what [`wants`] lists after every
//! change of the network.

use std::collections::BTreeMap;
use std::sync::Arc;

use penumbra_core::chan::MAX_CHANNELS;
use penumbra_core::{Channel, Chans, Error, Name, ParamSpec, Table, Unit};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, Network, NodeDesc, NodeTypes, Operator,
};
use penumbra_http::hub::{Hub, Params, Publisher, Want, Watch};

use crate::osc::{Filter, Made, channels_of};

/// Kind of the HTTP input node.
pub const HTTP_IN: &str = "chan.http_in";
/// The default port: free on most machines and short to type on a phone.
pub const DEFAULT_PORT: i64 = 8090;
/// The default folder of the control page, in the project.
pub const DEFAULT_FOLDER: &str = "www";

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            HTTP_IN,
            "A control page in the browser (a phone on the LAN) or another program sends JSON over HTTP or a WebSocket: every address a channel",
        )
        .block(penumbra_graph::Block::In)
        .param(
            ParamSpec::int("port", DEFAULT_PORT)
                .clamp(0.0, 65535.0)
                .range(1024.0, 65535.0)
                .special(0.0, "any free port")
                .doc("TCP port to listen on. The page opens at `http://localhost:8090/` for the default; http.status names the address and the port it got"),
        )
        .param(
            ParamSpec::choice("bind", &["localhost", "lan"])
                .doc("Who may connect")
                .choice_doc("localhost", "this machine only")
                .choice_doc("lan", "other devices too (a phone on the same network); Windows may ask to allow it"),
        )
        .param(ParamSpec::text("addresses", "*").doc(
            "Space-separated OSC patterns of the addresses to keep as channels: `/1/* /xy`; `*` all; `^/ping` leaves an address out",
        ))
        .param(
            ParamSpec::choice("params", &["controls", "off", "all"])
                .doc("Which params the addresses may set (live, outside the undo history)")
                .choice_doc("controls", "the value of a chan.slider, chan.button or chan.dropdown flagged `show`: `/fader/value 0.5`, `/look/value maze`")
                .choice_doc("off", "no param: the messages make channels only")
                .choice_doc("all", "any param the address names, as chan.osc_in's `params`: `/scene/speed 0.5`"),
        )
        .param(ParamSpec::text("folder", DEFAULT_FOLDER).doc(
            "Folder in the project whose files the page and its scripts come from; / opens its index.html. Empty: no files",
        ))
        .param(ParamSpec::bool("page", true).doc(
            "Without an index.html in `folder`, / opens a built-in page with every control flagged `show`, as on the show screen. Off: / answers 404 then",
        ))
        .param(
            ParamSpec::float("rate", 30.0)
                .clamp(1.0, 120.0)
                .range(1.0, 60.0)
                .unit(Unit::Hertz)
                .doc("State messages per second to each WebSocket client, at most (only when something changed)"),
        )
        .param(ParamSpec::text("table", "").doc(
            "The address whose table the `table` output holds: `/cues`. Empty: the table that came last",
        ))
        .input("chans", DataKind::Chan)
        .describe_input("chans", "Channels sent back to the page with every state message, under `chans`")
        .output("out", DataKind::Chan)
        .output("messages", DataKind::Table)
        .output("table", DataKind::Table)
        .describe_output("out", "A channel per address (per number), holding its latest value")
        .describe_output("messages", "What arrived: address, count, values")
        .describe_output("table", "A table a message sent as an array of arrays or of objects"),
        || Box::new(HttpIn::default()) as Box<dyn Operator>,
    ))?;
    Ok(())
}

fn params_of(text: &str) -> Params {
    match text {
        "all" => Params::All,
        "off" => Params::Off,
        _ => Params::Controls,
    }
}

/// The listeners the network's `chan.http_in` nodes want (stored params; a binding of `port` or
/// `bind` is not followed: a listener does not move with a channel).
pub fn wants(net: &Network, types: &NodeTypes) -> Vec<Want> {
    let Some(desc) = types.desc(HTTP_IN) else { return Vec::new() };
    net.nodes()
        .filter(|n| n.kind == HTTP_IN)
        .filter_map(|n| {
            let port = n.param(desc, "port").and_then(|v| v.as_i64())?;
            let text = |name: &str| {
                n.param(desc, name).and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
            };
            let rate =
                n.param(desc, "rate").and_then(|v| v.as_f64()).filter(|r| r.is_finite()).unwrap_or(30.0);
            Some(Want {
                port: u16::try_from(port).ok()?,
                lan: text("bind") == "lan",
                params: params_of(&text("params")),
                // `as`: 1 to 120 Hz is 8 to 1000 ms.
                interval_ms: (1000.0 / rate.clamp(1.0, 120.0)).round() as u32,
            })
        })
        .collect()
}

fn hub_of<'a>(cx: &'a CookContext<'_>) -> Result<&'a Hub, CookError> {
    cx.resource::<Hub>().ok_or_else(|| CookError::failed("HTTP is not available in this process"))
}

#[derive(Default)]
struct HttpIn {
    watch: Option<Watch>,
    publisher: Option<Publisher>,
    /// The store and table versions and the filter and table texts of the last cook.
    seen: Option<(u64, u64, String, String)>,
}

impl Operator for HttpIn {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let hub = hub_of(cx)?;
        let port = u16::try_from(cx.int("port")?).map_err(|_| CookError::failed("`port` is 0 to 65535"))?;
        if self.watch.as_ref().is_none_or(|w| w.port() != port) {
            let waker = cx.waker();
            self.watch = Some(hub.watch(port, Arc::new(move || waker.wake())));
            self.publisher = Some(hub.publisher(port));
            self.seen = None;
        }
        if let Some(w) = &self.watch {
            w.ack();
        }
        let folder = cx.text("folder")?.trim();
        let folder = if folder.is_empty() { None } else { Some(cx.resolve_path(folder)?) };
        let chans: Vec<(String, f64)> = cx
            .input_as::<Chans>(0)
            .map(|c| c.channels().iter().map(|ch| (ch.name.to_string(), ch.value())).collect())
            .unwrap_or_default();
        if let Some(p) = &self.publisher {
            p.set(folder, cx.bool("page")?, chans);
        }
        let Some(status) = hub.listener(port) else {
            return Err(CookError::failed(if hub.config().enabled {
                format!("not listening on port {port} yet")
            } else {
                "HTTP input is switched off (http.config enabled)".to_string()
            }));
        };
        if let Some(e) = status.error {
            return Err(CookError::failed(e));
        }
        let store = hub.read(port).unwrap_or_default();
        let tables = hub.tables(port).unwrap_or_default();
        let text = cx.text("addresses")?;
        let table_name = cx.text("table")?.trim();
        let seen = (store.version, tables.version, text.to_string(), table_name.to_string());
        if !cx.params_changed() && !cx.first_cook() && self.seen.as_ref() == Some(&seen) {
            return Ok(Cooked::Unchanged);
        }
        self.seen = Some(seen);
        let filter = Filter::parse(text);
        let mut made = Made::new(MAX_CHANNELS);
        let mut taken: BTreeMap<String, String> = BTreeMap::new();
        let mut clashes = Vec::new();
        let mut rows = vec![vec!["address".to_string(), "count".to_string(), "values".to_string()]];
        for e in &store.entries {
            let mut row = vec![e.address.clone(), e.count.to_string()];
            row.extend(e.values.iter().map(|v| format!("{v}")));
            if let Some(t) = &e.text {
                row.push(t.clone());
            }
            rows.push(row);
            if !filter.admits(&e.address) {
                continue;
            }
            for (name, v) in channels_of(&e.address, e.values.len()).into_iter().zip(&e.values) {
                if let Some(first) = taken.get(&name) {
                    clashes.push(format!("{} and {first} both make `{name}`", e.address));
                    continue;
                }
                let Ok(n) = Name::new(&name) else { continue };
                if made.full() {
                    continue;
                }
                taken.insert(name, e.address.clone());
                made.channels.push(Channel { name: n, samples: vec![if v.is_finite() { *v } else { 0.0 }] });
            }
        }
        let wanted = if table_name.is_empty() {
            None
        } else if table_name.starts_with('/') {
            Some(table_name.to_string())
        } else {
            Some(format!("/{table_name}"))
        };
        let table = match &wanted {
            None => tables.list.last(),
            Some(a) => tables.list.iter().find(|(t, _)| t == a),
        };
        let table = table.map(|(_, rows)| rows.clone()).unwrap_or_default();
        let mut warnings = filter.bad.clone();
        if let Some(c) = clashes.first() {
            warnings.push(format!("{c}; the first is kept"));
        }
        warnings.extend(made.warning("`addresses`"));
        if !warnings.is_empty() {
            cx.warn(warnings.join("; "));
        }
        let chans = Chans::new(cx.rate().as_f64(), made.channels)?;
        Ok(Cooked::Outputs(vec![
            Data::new(DataKind::Chan, chans),
            Data::new(DataKind::Table, Table::new(rows)?),
            Data::new(DataKind::Table, Table::new(table)?),
        ]))
    }
}
