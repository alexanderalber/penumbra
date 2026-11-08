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
//! - **`out.osc`** sends its input's channels (and the rows of a table: address, then
//!   values) to a host and port at a rate, as a bundle per packet; by default only what
//!   changed, plus everything once a second.
//!
//! The listener itself belongs to the engine's [`Hub`], which opens what [`wants`] lists after
//! every change of the network, so a node nobody reads still listens (for learn and for params).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use penumbra_core::chan::MAX_CHANNELS;
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
pub const OSC_OUT: &str = "out.osc";
/// The default port to listen on: what TouchOSC sends to by default.
pub const DEFAULT_IN_PORT: i64 = 8000;
/// The default port to send to: what TouchOSC listens on by default.
pub const DEFAULT_OUT_PORT: i64 = 9000;
/// Largest packet sent: fits an Ethernet frame without fragmenting.
pub const SEND_PACKET: usize = 1400;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(OSC_IN, "OSC from apps, controllers and other programs (TouchOSC on a phone, say): every address a channel").block(penumbra_graph::Block::In)
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
            .param(ParamSpec::text("triggers", "").doc(
                "Space-separated OSC patterns of addresses that are presses, such as a desk's GO: \
                 `/go`. Their channel is 1 for one frame per press, then 0. A press is a message \
                 without a number or with 0.5 or more, so `/go 1` and `/go` count and `/go 0` does \
                 not. A sender that also sends `/go 0` presses again only after its next `/go 0`, so \
                 a held button that is sent again counts once. Two presses within one frame come out \
                 one after the other",
            ))
            .param(ParamSpec::bool("params", false).doc(
                "Also set the params the addresses name: `/scene/speed 0.5` sets /scene.speed (live, \
                 outside the undo history). For continuous control, bind params to the channels instead",
            ))
            .output("out", DataKind::Chan)
            .output("messages", DataKind::Table)
            .describe_output(
                "out",
                "A channel per address (per number), holding its latest value; a trigger address's \
                 channel is 1 for one frame per press",
            )
            .describe_output("messages", "What arrived: address, count, values"),
        || Box::new(OscIn::default()) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new(OSC_OUT, "Sends channels and table rows as OSC to another program or device")
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

/// Channels made from what arrives, at most `limit`. Past it the first ones stay and the rest
/// are counted, so a flood of new addresses leaves the channels a binding reads in place
/// instead of failing the node.
pub(crate) struct Made {
    pub(crate) channels: Vec<Channel>,
    limit: usize,
    left_out: usize,
}

impl Made {
    pub(crate) fn new(limit: usize) -> Self {
        Self { channels: Vec::new(), limit: limit.min(MAX_CHANNELS), left_out: 0 }
    }

    /// Whether no more channels fit; counts the one that was to come.
    pub(crate) fn full(&mut self) -> bool {
        let full = self.channels.len() >= self.limit;
        if full {
            self.left_out += 1;
        }
        full
    }

    /// The warning for the channels left out, naming the param that narrows them.
    pub(crate) fn warning(&self, narrow: &str) -> Option<String> {
        (self.left_out > 0).then(|| {
            format!("{} channels kept, {} more left out; narrow {narrow}", self.channels.len(), self.left_out)
        })
    }
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

/// Presses of one trigger address waiting to come out, at most: a desk's double press is two.
const MAX_QUEUED_PRESSES: u32 = 8;

/// One trigger address's presses as one-frame pulses.
#[derive(Clone, Debug, Default)]
struct Trigger {
    /// The address's press count in the store when last looked at.
    seen: u64,
    /// Presses not yet sent out as a pulse.
    queued: u32,
    /// The channel was 1 at the last cook: the next is 0.
    high: bool,
}

impl Trigger {
    /// The channel's value at this cook, from the store's press count now.
    fn step(&mut self, presses: u64) -> f64 {
        // A store that started over (the listener moved) counts from there.
        let new = presses.saturating_sub(self.seen);
        self.seen = presses;
        self.queued =
            self.queued.saturating_add(u32::try_from(new).unwrap_or(u32::MAX)).min(MAX_QUEUED_PRESSES);
        if self.high {
            self.high = false;
        } else if self.queued > 0 {
            self.queued -= 1;
            self.high = true;
        }
        if self.high { 1.0 } else { 0.0 }
    }

    /// Whether the node has to cook at the next frame for this trigger.
    fn busy(&self) -> bool {
        self.high || self.queued > 0
    }
}

#[derive(Default)]
struct OscIn {
    watch: Option<Watch>,
    /// The store version and filter texts of the last cook.
    seen: Option<(u64, String, String)>,
    /// Per trigger address, its pulses; started anew when `triggers` changes.
    triggers: BTreeMap<String, Trigger>,
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
        let trigger_text = cx.text("triggers")?;
        let busy = self.triggers.values().any(Trigger::busy);
        if !cx.params_changed()
            && !cx.first_cook()
            && !busy
            && self
                .seen
                .as_ref()
                .is_some_and(|(v, t, g)| *v == store.version && t == text && g == trigger_text)
        {
            return Ok(Cooked::Unchanged);
        }
        let retriggered = self.seen.as_ref().is_none_or(|(_, _, g)| g != trigger_text);
        self.seen = Some((store.version, text.to_string(), trigger_text.to_string()));
        let filter = Filter::parse(text);
        let trigger_filter = Filter::parse(trigger_text);
        if retriggered {
            // Presses that came before the address was a trigger are not played out.
            self.triggers = store
                .entries
                .iter()
                .filter(|e| trigger_filter.admits(&e.address))
                .map(|e| (e.address.clone(), Trigger { seen: e.presses, ..Trigger::default() }))
                .collect();
        }
        let mut made = Made::new(MAX_CHANNELS);
        let mut taken: BTreeMap<String, String> = BTreeMap::new();
        let mut clashes = Vec::new();
        // A trigger written out in full has its channel (at 0) before its first press, so a
        // binding finds it.
        for address in
            trigger_text.split_whitespace().filter(|a| a.starts_with('/') && !penumbra_osc::is_pattern(a))
        {
            if store.entries.iter().any(|e| e.address == address) {
                continue;
            }
            let Some(name) = channel_name(address) else { continue };
            let Ok(n) = Name::new(&name) else { continue };
            if taken.contains_key(&name) || made.full() {
                continue;
            }
            taken.insert(name, address.to_string());
            made.channels.push(Channel { name: n, samples: vec![0.0] });
        }
        let mut rows = vec![vec!["address".to_string(), "count".to_string(), "values".to_string()]];
        for e in &store.entries {
            let mut row = vec![e.address.clone(), e.count.to_string()];
            row.extend(e.values.iter().map(|v| format!("{v}")));
            if let Some(t) = &e.text {
                row.push(t.clone());
            }
            rows.push(row);
            if trigger_filter.admits(&e.address) {
                let value = self.triggers.entry(e.address.clone()).or_default().step(e.presses);
                let Some(name) = channel_name(&e.address) else { continue };
                if let Some(first) = taken.get(&name) {
                    clashes.push(format!("{} and {first} both make `{name}`", e.address));
                    continue;
                }
                let Ok(n) = Name::new(&name) else { continue };
                if made.full() {
                    continue;
                }
                taken.insert(name, e.address.clone());
                made.channels.push(Channel { name: n, samples: vec![value] });
                continue;
            }
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
        if self.triggers.values().any(Trigger::busy) {
            // The pulse has to fall again (or the next press come out) at the next frame.
            cx.waker().wake();
        }
        let mut warnings = filter.bad.clone();
        warnings.extend(trigger_filter.bad.iter().cloned());
        if let Some(c) = clashes.first() {
            warnings.push(format!("{c}; the first is kept"));
        }
        warnings.extend(made.warning("`addresses` or `triggers`"));
        if !warnings.is_empty() {
            cx.warn(warnings.join("; "));
        }
        let chans = Chans::new(cx.rate().as_f64(), made.channels)?;
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
    /// The last sending cook's warning, shown again on the frames between sends (a warning
    /// lasts one cook).
    warning: Option<String>,
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
            if let Some(w) = &self.warning {
                cx.warn(w.clone());
            }
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
            // Only what this send names is remembered: addresses that came and went do not
            // pile up.
            self.sent.clear();
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
        let mut warnings = Vec::new();
        if skipped > 0 {
            warnings.push(format!("{skipped} table rows skipped: an address has no spaces"));
        }
        if let Some(e) = errors.first() {
            warnings.push(format!("sending to {target}: {e}"));
        }
        self.warning = (!warnings.is_empty()).then(|| warnings.join("; "));
        if let Some(w) = &self.warning {
            cx.warn(w.clone());
        }
        Ok(Cooked::Outputs(Vec::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::Trigger;

    #[test]
    fn presses_become_one_frame_pulses_one_after_the_other() {
        let mut t = Trigger { seen: 4, ..Trigger::default() };
        // Nothing new: 0, and nothing to do at the next frame.
        assert_eq!(t.step(4), 0.0);
        assert!(!t.busy());
        // Two presses within one frame: 1, 0, 1, 0.
        let pulses: Vec<f64> = [6, 6, 6, 6, 6].iter().map(|&p| t.step(p)).collect();
        assert_eq!(pulses, [1.0, 0.0, 1.0, 0.0, 0.0]);
        assert!(!t.busy());
        // A press while the pulse is up comes out after it falls.
        assert_eq!(t.step(7), 1.0);
        assert_eq!(t.step(8), 0.0);
        assert_eq!(t.step(8), 1.0);
        assert_eq!(t.step(8), 0.0);
        // A burst is capped; a store that started over counts from its new count.
        t.step(1000);
        assert!(t.queued < super::MAX_QUEUED_PRESSES);
        assert_eq!(t.step(2), 0.0);
        assert_eq!(t.seen, 2);
    }
}
