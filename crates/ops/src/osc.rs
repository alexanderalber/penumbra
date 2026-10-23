//! OSC nodes.
//!
//! - **`chan.osc_in`** listens on a UDP port (this machine only unless `lan`) and makes every
//!   address that arrives a channel holding its latest number: `/fader1 0.5` becomes channel
//!   `fader1`, an address with several numbers one channel per number (`xy_0`, `xy_1`), in the
//!   order the addresses first came. Binding params to these channels is the primary way to
//!   drive the show from outside. With `params` on, the same messages also set the params whose
//!   path their address names (the engine does that, see `penumbra-engine`'s `osc` module).
//!   The node recooks when something arrives, not every frame. Its second output lists what
//!   arrived as a table.
//! - **`chan.osc_out`** sends its input's channels (and the rows of a table: address, then
//!   values) to a host and port at a rate, as a bundle per packet; by default only what
//!   changed, plus everything once a second.
//!
//! The listener itself belongs to the engine's [`Hub`], which opens what [`wants`] lists after
//! every change of the network, so a node nobody reads still listens (for learn and for params).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use penumbra_core::{Channel, Chans, Error, Name, ParamSpec, Table, Unit};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, Network, NodeDesc, NodeTypes, Operator,
};
use penumbra_osc::codec::{self, Arg, Message};
use penumbra_osc::hub::{self, Hub, Sender, Want, Watch};
use penumbra_osc::{Budget, Pattern, channel_name};

/// Kind of the OSC input node.
pub const OSC_IN: &str = "chan.osc_in";
/// Kind of the OSC output node.
pub const OSC_OUT: &str = "chan.osc_out";
/// The default port to listen on: what TouchOSC sends to by default.
pub const DEFAULT_IN_PORT: i64 = 8000;
/// The default port to send to: what TouchOSC listens on by default.
pub const DEFAULT_OUT_PORT: i64 = 9000;
/// Largest packet sent: fits an Ethernet frame without fragmenting.
pub const SEND_PACKET: usize = 1400;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(OSC_IN, "OSC from the network: every address a channel with its latest number")
            .param(
                ParamSpec::int("port", DEFAULT_IN_PORT)
                    .clamp(0.0, 65535.0)
                    .range(1024.0, 65535.0)
                    .special(0.0, "any free port")
                    .doc("UDP port to listen on; osc.status says which port it got"),
            )
            .param(ParamSpec::bool("lan", false).doc(
                "Also take OSC from other devices (phones, other computers): listens on every network \
                     interface. Off: this machine only",
            ))
            .param(ParamSpec::text("addresses", "*").doc(
                "Space-separated OSC patterns of the addresses to keep as channels: `/1/* /xy`; `*` all; \
                 `^/ping` leaves an address out",
            ))
            .param(ParamSpec::bool("params", false).doc(
                "Also set the params the addresses name: `/scene/speed 0.5` sets /scene.speed (live, \
                 outside the undo history). For continuous control, bind params to the channels instead",
            ))
            .output("out", DataKind::Chan)
            .output("messages", DataKind::Table)
            .describe_output("out", "A channel per address (per number), holding its latest value")
            .describe_output("messages", "What arrived: address, count, values"),
        || Box::new(OscIn::default()) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new(OSC_OUT, "Sends channels (and table rows: address, values) as OSC to a host and port")
            .param(
                ParamSpec::text("host", "127.0.0.1")
                    .doc("IP address to send to, or localhost; names are not looked up"),
            )
            .param(
                ParamSpec::int("port", DEFAULT_OUT_PORT)
                    .clamp(1.0, 65535.0)
                    .range(1024.0, 65535.0)
                    .doc("UDP port the receiver listens on (TouchOSC and many apps use 9000)"),
            )
            .param(
                ParamSpec::float("rate", 30.0)
                    .clamp(0.1, 1000.0)
                    .range(1.0, 120.0)
                    .unit(Unit::Hertz)
                    .doc("Sends per second at most"),
            )
            .param(
                ParamSpec::choice("send", &["changes", "always"])
                    .doc("Which channels each send carries")
                    .choice_doc("changes", "only what changed, plus everything once a second")
                    .choice_doc("always", "everything, each time"),
            )
            .param(
                ParamSpec::text("prefix", "")
                    .doc("Put before every channel's address: `/show` sends /show/x"),
            )
            .param(
                ParamSpec::bool("bundle", true)
                    .more()
                    .doc("Several messages per packet as a bundle; off sends one packet per message"),
            )
            .input("chans", DataKind::Chan)
            .input("table", DataKind::Table)
            .time_dependent(),
        || Box::new(OscOut::default()) as Box<dyn Operator>,
    ))?;
    Ok(())
}

/// The listeners the network's `chan.osc_in` nodes want (stored params; a binding of `port` or
/// `lan` is not followed: a listener does not move with a channel).
pub fn wants(net: &Network, types: &NodeTypes) -> Vec<Want> {
    let Some(desc) = types.desc(OSC_IN) else { return Vec::new() };
    net.nodes()
        .filter(|n| n.kind == OSC_IN)
        .filter_map(|n| {
            let port = n.param(desc, "port").and_then(|v| v.as_i64())?;
            Some(Want {
                port: u16::try_from(port).ok()?,
                lan: n.param(desc, "lan").and_then(|v| v.as_bool()).unwrap_or(false),
                params: n.param(desc, "params").and_then(|v| v.as_bool()).unwrap_or(false),
            })
        })
        .collect()
}

/// `chan.osc_in`'s `addresses`: OSC patterns to keep and to leave out (`^`). `*` on its own
/// keeps every address, whatever its depth.
#[derive(Default)]
pub struct Filter {
    all: bool,
    keep: Vec<Pattern>,
    drop: Vec<Pattern>,
    /// Patterns that did not compile, for a warning.
    pub bad: Vec<String>,
}

/// Steps one address may take through a filter.
const FILTER_STEPS: u64 = 50_000;

impl Filter {
    pub fn parse(text: &str) -> Filter {
        let mut f = Filter::default();
        for token in text.split_whitespace() {
            let (leave_out, p) = match token.strip_prefix('^') {
                Some(p) => (true, p),
                None => (false, token),
            };
            if p == "*" {
                if !leave_out {
                    f.all = true;
                }
                continue;
            }
            match Pattern::compile(p) {
                Ok(c) if leave_out => f.drop.push(c),
                Ok(c) => f.keep.push(c),
                Err(e) => f.bad.push(format!("`{token}`: {e}")),
            }
        }
        f
    }

    pub fn admits(&self, address: &str) -> bool {
        let mut budget = Budget(FILTER_STEPS);
        (self.all || self.keep.iter().any(|p| p.matches(address, &mut budget)))
            && !self.drop.iter().any(|p| p.matches(address, &mut budget))
    }
}

/// The channels for one address with `n` numbers: its name for one, `name_k` for several.
pub fn channels_of(address: &str, n: usize) -> Vec<String> {
    let Some(base) = channel_name(address) else { return Vec::new() };
    match n {
        0 => Vec::new(),
        1 => vec![base],
        n => (0..n).map(|k| format!("{base}_{k}")).collect(),
    }
}

fn hub_of<'a>(cx: &'a CookContext<'_>) -> Result<&'a Hub, CookError> {
    cx.resource::<Hub>().ok_or_else(|| CookError::failed("OSC is not available in this process"))
}

fn port_of(cx: &CookContext<'_>) -> Result<u16, CookError> {
    u16::try_from(cx.int("port")?).map_err(|_| CookError::failed("`port` is 0 to 65535"))
}

#[derive(Default)]
struct OscIn {
    watch: Option<Watch>,
    /// The store version and filter text of the last cook.
    seen: Option<(u64, String)>,
}

impl Operator for OscIn {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let hub = hub_of(cx)?;
        let port = port_of(cx)?;
        if self.watch.as_ref().is_none_or(|w| w.port() != port) {
            let waker = cx.waker();
            self.watch = Some(hub.watch(port, Arc::new(move || waker.wake())));
            self.seen = None;
        }
        if let Some(w) = &self.watch {
            w.ack();
        }
        let Some(status) = hub.listener(port) else {
            return Err(CookError::failed(if hub.config().enabled {
                format!("not listening on port {port} yet")
            } else {
                "OSC input is switched off (osc.config enabled)".to_string()
            }));
        };
        if let Some(e) = status.error {
            return Err(CookError::failed(e));
        }
        let store = hub.read(port).unwrap_or_default();
        let text = cx.text("addresses")?;
        if !cx.params_changed()
            && !cx.first_cook()
            && self.seen.as_ref().is_some_and(|(v, t)| *v == store.version && t == text)
        {
            return Ok(Cooked::Unchanged);
        }
        self.seen = Some((store.version, text.to_string()));
        let filter = Filter::parse(text);
        let mut channels: Vec<Channel> = Vec::new();
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
                taken.insert(name, e.address.clone());
                channels.push(Channel { name: n, samples: vec![if v.is_finite() { *v } else { 0.0 }] });
            }
        }
        let mut warnings = filter.bad.clone();
        if let Some(c) = clashes.first() {
            warnings.push(format!("{c}; the first is kept"));
        }
        if !warnings.is_empty() {
            cx.warn(warnings.join("; "));
        }
        let chans = Chans::new(cx.rate().as_f64(), channels)?;
        Ok(Cooked::Outputs(vec![
            Data::new(DataKind::Chan, chans),
            Data::new(DataKind::Table, Table::new(rows)?),
        ]))
    }
}

#[derive(Default)]
struct OscOut {
    sender: Option<Sender>,
    /// Show time of the last send, in seconds.
    last: Option<f64>,
    /// Show time of the last full send.
    last_full: Option<f64>,
    /// What was sent last, per address.
    sent: BTreeMap<String, Vec<Arg>>,
}

/// `prefix` as an address start: empty, or `/a/b` without a trailing slash.
fn prefix_of(text: &str) -> Result<String, CookError> {
    let p = text.trim().trim_end_matches('/');
    if p.is_empty() {
        return Ok(String::new());
    }
    if !p.starts_with('/') || p.contains(char::is_whitespace) {
        return Err(CookError::failed("`prefix` starts with / and has no spaces: /show"));
    }
    Ok(p.to_string())
}

/// A table cell as an arg: a number as a float, anything else as a string.
fn cell_arg(cell: &str) -> Arg {
    match cell.trim().parse::<f64>() {
        // `as` is the conversion wanted: OSC floats are 32 bits.
        Ok(v) if v.is_finite() => Arg::Float(v as f32),
        _ => Arg::Str(cell.to_string()),
    }
}

impl Operator for OscOut {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let hub = hub_of(cx)?;
        let port = u16::try_from(cx.int("port")?).map_err(|_| CookError::failed("`port` is 1 to 65535"))?;
        let target: SocketAddr = hub::target(cx.text("host")?, port).map_err(CookError::failed)?;
        if self.sender.as_ref().is_none_or(|s| s.target() != target) {
            self.sender = None;
            self.sent.clear();
            let s = hub
                .sender(&format!("{OSC_OUT} {}", cx.node()), target)
                .map_err(|e| CookError::failed(format!("cannot open a socket to send to {target}: {e}")))?;
            self.sender = Some(s);
        }
        if cx.input(0).is_none() && cx.input(1).is_none() {
            return Err(crate::waiting(cx));
        }
        let now = cx.time().seconds();
        let period = 1.0 / cx.float("rate")?.max(0.1);
        // A little slack, so 30 Hz at 60 fps sends every second frame and not every third.
        if let Some(last) = self.last
            && now >= last
            && now - last < period * 0.9
            && !cx.params_changed()
        {
            return Ok(Cooked::Outputs(Vec::new()));
        }
        self.last = Some(now);
        let prefix = prefix_of(cx.text("prefix")?)?;
        let mut messages: Vec<Message> = Vec::new();
        if let Some(chans) = cx.input_as::<Chans>(0) {
            for ch in chans.channels() {
                // `as`: OSC floats are 32 bits.
                messages
                    .push(Message::new(format!("{prefix}/{}", ch.name), vec![Arg::Float(ch.value() as f32)]));
            }
        }
        let mut skipped = 0usize;
        if let Some(table) = cx.input_as::<Table>(1) {
            for row in table.rows() {
                let Some((first, rest)) = row.split_first() else { continue };
                let first = first.trim();
                if first.is_empty() {
                    continue;
                }
                let address = if first.starts_with('/') {
                    format!("{prefix}{first}")
                } else {
                    format!("{prefix}/{first}")
                };
                if address.contains(char::is_whitespace) || address.len() > 512 {
                    skipped += 1;
                    continue;
                }
                messages.push(Message::new(address, rest.iter().map(|c| cell_arg(c)).collect()));
            }
        }
        let full = cx.text("send")? == "always"
            || self.last_full.is_none_or(|t| now < t || now - t >= 1.0)
            || cx.params_changed();
        if full {
            self.last_full = Some(now);
        } else {
            messages.retain(|m| self.sent.get(&m.address) != Some(&m.args));
        }
        for m in &messages {
            self.sent.insert(m.address.clone(), m.args.clone());
        }
        let mut errors = Vec::new();
        if let Some(sender) = &self.sender {
            let bundle = cx.bool("bundle")?;
            for packet in codec::packets(&messages, SEND_PACKET, bundle) {
                let count = if bundle { messages.len() } else { 1 };
                if let Err(e) = sender.send(&packet, count) {
                    errors.push(e.to_string());
                }
            }
        }
        if skipped > 0 {
            cx.warn(format!("{skipped} table rows skipped: an address has no spaces"));
        }
        if let Some(e) = errors.first() {
            cx.warn(format!("sending to {target}: {e}"));
        }
        Ok(Cooked::Outputs(Vec::new()))
    }
}
