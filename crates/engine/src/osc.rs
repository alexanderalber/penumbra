//! OSC in the engine: the hub's listeners follow the network's `chan.oscin` nodes (the cook thread
//! calls [`penumbra_ops::osc::wants`] when the network changes), and the **osc thread** turns what
//! arrives into edits:
//!
//! - **Params by address** (on ports whose `chan.oscin` has `params` on): `/scene/speed 0.5`
//!   sets `/scene.speed` (an exposed param of a component, or any node's param by its path;
//!   `/n/translate/0 0.5` sets one component; `/n/translate 1 2 3` the whole vector). Patterns
//!   in the address (`/lights/*/dim 0`) match nodes and params with a step budget. What arrives
//!   within one frame becomes one edit. The values are live control, as in TouchDesigner: they
//!   do not enter the history, and undo or redo of other edits keeps them
//!   ([`penumbra_graph::History::live`]). OSC edits pass the gate as the user's (refused
//!   while Claude holds the claim) and are refused on locked nodes and bound params.
//! - **Learn** (`osc.learn`): the next address with a number binds the param to that address's
//!   channel on the `chan.oscin` that takes it; one undo step.
//!
//! Commands: `osc.status`, `osc.config`, `osc.learn`, `osc.send`.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use penumbra_core::{Name, NodeId, NodePath, ParamKind, ParamPath, ParamSpec, Value};
use penumbra_graph::{Network, NodeDef, NodeTypes, Op};
use penumbra_ops::osc::{Filter, OSC_IN, channels_of};
use penumbra_osc::codec::{self, Arg, Message};
use penumbra_osc::hub::{self, Config, Hub, Incoming};
use penumbra_osc::{Budget, Pattern};
use penumbra_project::{Actor, Session};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use crate::command::{CommandError, FnCommand, Registry, args};
use crate::engine::Engine;

/// At most one edit per this (about a frame), so a burst becomes one edit.
pub const APPLY_PERIOD: Duration = Duration::from_millis(16);
/// Steps one pattern address may take through the network.
pub const PATTERN_STEPS: u64 = 200_000;
/// Params one message may set.
pub const MAX_TARGETS: usize = 64;
/// Nodes a pattern may visit.
pub const MAX_VISITS: usize = 20_000;
/// Default and longest wait of `osc.learn`.
pub const LEARN_TIMEOUT: Duration = Duration::from_secs(30);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What OSC did to params, for `osc.status` and the Helpers window.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ParamStats {
    /// Param values set.
    pub applied: u64,
    /// Messages refused (gate, lock, binding, a value that does not fit).
    pub refused: u64,
    /// Messages whose address names no param (probably meant for the channels).
    pub unmatched: u64,
    pub last_applied: Option<String>,
    pub last_refused: Option<String>,
}

/// An armed learn.
#[derive(Clone, Debug, Serialize)]
pub struct Learning {
    pub param: String,
    #[serde(skip)]
    target: (ParamPath, Option<u8>),
    #[serde(skip)]
    until: Instant,
    /// Seconds left.
    pub remaining: f64,
}

/// How the last learn ended.
#[derive(Clone, Debug, Serialize)]
pub struct Learned {
    pub param: String,
    pub address: Option<String>,
    pub from: Option<String>,
    pub channels: Vec<String>,
    pub error: Option<String>,
}

/// Shared between the osc thread, the commands and the UI.
pub struct OscShared {
    hub: Hub,
    stop: AtomicBool,
    stats: Mutex<ParamStats>,
    learning: Mutex<Option<Learning>>,
    learned: Mutex<Option<Learned>>,
}

impl OscShared {
    pub fn new(hub: Hub) -> Self {
        Self {
            hub,
            stop: AtomicBool::new(false),
            stats: Mutex::new(ParamStats::default()),
            learning: Mutex::new(None),
            learned: Mutex::new(None),
        }
    }

    pub fn hub(&self) -> &Hub {
        &self.hub
    }

    pub fn stats(&self) -> ParamStats {
        lock(&self.stats).clone()
    }

    pub fn learning(&self) -> Option<Learning> {
        let now = Instant::now();
        lock(&self.learning).clone().map(|mut l| {
            l.remaining = l.until.saturating_duration_since(now).as_secs_f64();
            l
        })
    }

    pub fn learned(&self) -> Option<Learned> {
        lock(&self.learned).clone()
    }

    /// Arms learn for `param` (`/n.scale`, `/n.translate.0`, or a vector as a whole).
    pub fn learn(&self, session: &Session, param: &str, timeout: Duration) -> Result<(), String> {
        let (path, component) = crate::commands::param_part(param).map_err(|e| e.0)?;
        let net = session.doc().snapshot();
        let types = session.types();
        let target = net.resolve_alias(types, &path).map_err(|e| e.0)?;
        let (_, spec) = param_spec(&net, types, &target)?;
        if spec.bind_components() == 0 {
            return Err(format!("`{}` is {}; channels drive numbers only", spec.name, spec.kind_label()));
        }
        if component.is_some() {
            spec.check_bindable(component).map_err(|e| e.0)?;
        }
        *lock(&self.learning) = Some(Learning {
            param: param.to_string(),
            target: (path, component),
            until: Instant::now() + timeout.min(Duration::from_secs(600)),
            remaining: 0.0,
        });
        self.hub.learn(true);
        Ok(())
    }

    pub fn cancel_learn(&self) -> bool {
        self.hub.learn(false);
        lock(&self.learning).take().is_some()
    }

    /// Stops the osc thread.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.hub.notify();
    }

    /// Lets the next osc thread run (another project opened).
    pub(crate) fn resume(&self) {
        self.stop.store(false, Ordering::Release);
    }
}

/// The osc thread's loop: waits for messages, applies them at most once per [`APPLY_PERIOD`].
pub fn run(session: &Session, shared: &OscShared) {
    while !shared.stop.load(Ordering::Acquire) {
        shared.hub.wait_params(Duration::from_millis(50));
        if shared.stop.load(Ordering::Acquire) {
            break;
        }
        let started = Instant::now();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            learn_step(session, shared, started);
            let incoming = shared.hub.take_params();
            if !incoming.is_empty() {
                apply(session, shared, &incoming, started);
            }
        }));
        if outcome.is_err() {
            log::error!("OSC: applying messages panicked; continuing");
        }
        let spent = started.elapsed();
        if spent < APPLY_PERIOD {
            std::thread::sleep(APPLY_PERIOD.saturating_sub(spent));
        }
    }
}

/// One param a message sets.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Target {
    pub node: NodeId,
    pub param: ParamPath,
    pub component: Option<u8>,
}

fn param_spec<'a>(
    net: &'a Network,
    types: &'a NodeTypes,
    param: &ParamPath,
) -> Result<(&'a Arc<NodeDef>, &'a ParamSpec), String> {
    let id = net.resolve(param.node()).ok_or_else(|| format!("no node at {}", param.node()))?;
    let node = net.node(id).ok_or_else(|| format!("no node at {}", param.node()))?;
    let desc = types.desc(&node.kind).ok_or_else(|| format!("unknown node kind `{}`", node.kind))?;
    let spec = desc.param_spec(param.param().as_str()).ok_or_else(|| {
        format!("`{}` has no param `{}` (or it is declared by its file)", node.kind, param.param())
    })?;
    Ok((node, spec))
}

/// The params `address` names: `/a/b/c` is param `c` of node `/a/b` (or the alias `c` of
/// component `/a/b`), else component `c` (0 to 3) of param `b` of `/a`. Patterns walk the tree
/// within the budget.
pub fn targets(net: &Network, types: &NodeTypes, address: &str, budget: &mut Budget) -> Vec<Target> {
    let mut out: BTreeSet<Target> = BTreeSet::new();
    let Ok(pattern) = Pattern::compile(address) else { return Vec::new() };
    let depth = pattern.depth();
    // Param as the last part.
    if depth >= 2 {
        for (node, name) in params_at(net, types, &pattern, depth - 1, budget) {
            if let Some(t) = target(net, types, node, &name, None) {
                out.insert(t);
            }
            if out.len() >= MAX_TARGETS {
                break;
            }
        }
    }
    // A component as the last part.
    if depth >= 3 && out.len() < MAX_TARGETS {
        let component = pattern.literal_part(depth - 1).and_then(|c| c.parse::<u8>().ok()).filter(|c| *c < 4);
        if let Some(c) = component {
            for (node, name) in params_at(net, types, &pattern, depth - 2, budget) {
                if let Some(t) = target(net, types, node, &name, Some(c)) {
                    out.insert(t);
                }
                if out.len() >= MAX_TARGETS {
                    break;
                }
            }
        }
    }
    out.into_iter().take(MAX_TARGETS).collect()
}

/// `(node, param name)` for nodes `levels` deep whose path matches the pattern's first parts
/// and params (and exposed aliases) matching part `levels`.
fn params_at(
    net: &Network,
    types: &NodeTypes,
    pattern: &Pattern,
    levels: usize,
    budget: &mut Budget,
) -> Vec<(NodeId, String)> {
    let mut frontier: Vec<Option<NodeId>> = vec![None];
    let mut visits = 0usize;
    for level in 0..levels {
        let mut next = Vec::new();
        for parent in &frontier {
            // A literal part needs no walk over every sibling.
            if let Some(name) = pattern.literal_part(level) {
                let Ok(name) = Name::new(&name) else { return Vec::new() };
                let path = match parent.and_then(|p| net.path_of(p)) {
                    Some(p) => p.child(name),
                    None => NodePath::root().child(name),
                };
                if let Some(id) = path.ok().and_then(|p| net.resolve(&p)) {
                    next.push(Some(id));
                }
                continue;
            }
            for child in net.children(*parent) {
                visits += 1;
                if visits > MAX_VISITS || budget.exhausted() {
                    return Vec::new();
                }
                if pattern.matches_part(level, child.name.as_str(), budget) {
                    next.push(Some(child.id));
                }
            }
        }
        frontier = next;
        if frontier.is_empty() {
            return Vec::new();
        }
    }
    let mut out = Vec::new();
    for id in frontier.into_iter().flatten() {
        let Some(node) = net.node(id) else { continue };
        let Some(desc) = types.desc(&node.kind) else { continue };
        let mut names: Vec<String> = desc.params.iter().map(|p| p.name.as_str().to_string()).collect();
        if desc.container {
            names.extend(net.exposed(types, id).into_iter().map(|e| e.name.as_str().to_string()));
        }
        for name in names {
            if pattern.matches_part(levels, &name, budget) {
                out.push((id, name));
            }
        }
    }
    out
}

fn target(
    net: &Network,
    types: &NodeTypes,
    node: NodeId,
    name: &str,
    component: Option<u8>,
) -> Option<Target> {
    let path = net.path_of(node)?.param(Name::new(name).ok()?).ok()?;
    let resolved = net.resolve_alias(types, &path).ok()?;
    let id = net.resolve(resolved.node())?;
    Some(Target { node: id, param: resolved, component })
}

/// The value `args` give `spec` (whose stored value is `current`).
pub fn value_for(
    spec: &ParamSpec,
    current: &Value,
    component: Option<u8>,
    args: &[Arg],
) -> Result<Value, String> {
    let numbers: Vec<f64> = args.iter().filter_map(Arg::as_f64).collect();
    let text = args.iter().find_map(Arg::as_str);
    let first = || numbers.first().copied().ok_or_else(|| format!("`{}` needs a number", spec.name));
    let e = |e: penumbra_core::Error| e.0;
    match &spec.kind {
        ParamKind::Bool | ParamKind::Int | ParamKind::Float => {
            spec.bind_value(current, None, first()?).map_err(e)
        }
        ParamKind::Vec(_) | ParamKind::Color => match component {
            Some(c) => spec.bind_value(current, Some(c), first()?).map_err(e),
            None => {
                if numbers.is_empty() {
                    return Err(format!("`{}` needs numbers", spec.name));
                }
                let mut v = current.clone();
                for (c, x) in numbers.iter().take(usize::from(spec.bind_components())).enumerate() {
                    let c = u8::try_from(c).unwrap_or(0);
                    v = spec.bind_value(&v, Some(c), *x).map_err(e)?;
                }
                Ok(v)
            }
        },
        ParamKind::Choice(choices) => match (text, numbers.first()) {
            (Some(t), _) => Ok(Value::Text(t.to_string())),
            (None, Some(i)) if i.is_finite() && *i >= 0.0 => {
                // `as` saturates; an index past the end is refused below.
                let i = i.round() as usize;
                choices
                    .get(i)
                    .map(|c| Value::Text(c.clone()))
                    .ok_or_else(|| format!("`{}` has {} choices", spec.name, choices.len()))
            }
            _ => Err(format!("`{}` needs a word or an index", spec.name)),
        },
        ParamKind::Text => {
            text.map(|t| Value::Text(t.to_string())).ok_or_else(|| format!("`{}` needs a string", spec.name))
        }
        _ => Err(format!("`{}` is {}; OSC does not set it", spec.name, spec.kind_label())),
    }
}

fn refuse(stats: &mut ParamStats, why: String) {
    stats.refused += 1;
    stats.last_refused = Some(why);
}

/// Turns one frame's messages into one edit.
fn apply(session: &Session, shared: &OscShared, incoming: &[Incoming], now: Instant) {
    let net = session.doc().snapshot();
    let types = session.types();
    let mut ops: Vec<Op> = Vec::new();
    let mut set: Vec<String> = Vec::new();
    let mut stats = lock(&shared.stats).clone();
    // Values built so far per param, so two messages for two components of one vector add up.
    let mut pending: std::collections::BTreeMap<ParamPath, Value> = std::collections::BTreeMap::new();
    for m in incoming {
        let mut budget = Budget(PATTERN_STEPS);
        let found = targets(&net, types, &m.address, &mut budget);
        if found.is_empty() {
            stats.unmatched += 1;
            continue;
        }
        for t in found {
            let Some(node) = net.node(t.node) else { continue };
            let label = match t.component {
                Some(c) => format!("{}.{c}", t.param),
                None => t.param.to_string(),
            };
            if node.locked {
                refuse(&mut stats, format!("{} from {}: the node is locked", label, m.address));
                continue;
            }
            let name = t.param.param().clone();
            let bound = node.binds.keys().any(|p| {
                p.param == name
                    && (t.component.is_none() || p.component.is_none() || p.component == t.component)
            });
            if bound {
                refuse(&mut stats, format!("{label} from {}: bound to a channel", m.address));
                continue;
            }
            let Ok((_, spec)) = param_spec(&net, types, &t.param) else {
                stats.unmatched += 1;
                continue;
            };
            let Some(desc) = types.desc(&node.kind) else { continue };
            let current = pending
                .get(&t.param)
                .cloned()
                .or_else(|| node.param(desc, name.as_str()).cloned())
                .unwrap_or_else(|| spec.default.clone());
            match value_for(spec, &current, t.component, &m.args) {
                Ok(v) => {
                    pending.insert(t.param.clone(), v);
                    set.push(label);
                }
                Err(e) => refuse(&mut stats, format!("{label} from {}: {e}", m.address)),
            }
        }
    }
    for (param, value) in pending {
        ops.push(Op::Set { param, value });
    }
    if !ops.is_empty() {
        match session.edit_live(&ops, now) {
            Ok(_) => {
                stats.applied += ops.len() as u64;
                stats.last_applied = Some(set.join(", "));
            }
            Err(e) => refuse(&mut stats, e.0),
        }
    }
    *lock(&shared.stats) = stats;
}

/// Ends a learn that heard something or ran out of time.
fn learn_step(session: &Session, shared: &OscShared, now: Instant) {
    let Some(learning) = lock(&shared.learning).clone() else {
        // Something heard after a cancel is dropped.
        shared.hub.take_heard();
        return;
    };
    let heard = shared.hub.take_heard();
    let finish = |l: Learned| {
        *lock(&shared.learning) = None;
        shared.hub.learn(false);
        *lock(&shared.learned) = Some(l);
    };
    let Some(heard) = heard else {
        if now >= learning.until {
            finish(Learned {
                param: learning.param.clone(),
                address: None,
                from: None,
                channels: Vec::new(),
                error: Some("nothing arrived in time".into()),
            });
        }
        return;
    };
    let result = bind_learned(session, &learning, &heard.address, heard.port, heard.values);
    let (from, channels, error) = match result {
        Ok((from, channels)) => (Some(from), channels, None),
        Err(e) => (None, Vec::new(), Some(e)),
    };
    finish(Learned { param: learning.param.clone(), address: Some(heard.address), from, channels, error });
}

fn bind_learned(
    session: &Session,
    learning: &Learning,
    address: &str,
    port: u16,
    values: usize,
) -> Result<(String, Vec<String>), String> {
    let net = session.doc().snapshot();
    let types = session.types();
    let desc = types.desc(OSC_IN).ok_or("no chan.oscin node type")?;
    let mut nodes: Vec<(NodePath, &Arc<NodeDef>)> = net
        .nodes()
        .filter(|n| n.kind == OSC_IN)
        .filter(|n| n.param(desc, "port").and_then(Value::as_i64) == Some(i64::from(port)))
        .filter(|n| {
            Filter::parse(n.param(desc, "addresses").and_then(Value::as_str).unwrap_or("*")).admits(address)
        })
        .filter_map(|n| Some((net.path_of(n.id)?, n)))
        .collect();
    nodes.sort_by(|a, b| a.0.cmp(&b.0));
    let Some((from, _)) = nodes.first() else {
        return Err(format!("no chan.oscin on port {port} keeps {address} (see its addresses)"));
    };
    let (param, component) = &learning.target;
    let target = net.resolve_alias(types, param).map_err(|e| e.0)?;
    let (_, spec) = param_spec(&net, types, &target)?;
    let channels = channels_of(address, values.max(1));
    let mut ops = Vec::new();
    let mut used = Vec::new();
    let comps = spec.bind_components();
    match (component, comps) {
        (Some(c), _) => {
            let ch = channels.first().ok_or("no channel")?;
            ops.push(bind(param, Some(*c), from, ch)?);
            used.push(ch.clone());
        }
        (None, 1) => {
            let ch = channels.first().ok_or("no channel")?;
            ops.push(bind(param, None, from, ch)?);
            used.push(ch.clone());
        }
        (None, n) => {
            for (c, ch) in channels.iter().take(usize::from(n)).enumerate() {
                ops.push(bind(param, Some(u8::try_from(c).unwrap_or(0)), from, ch)?);
                used.push(ch.clone());
            }
        }
    }
    session
        .edit(Actor::Osc, &ops, None, &format!("learn {} from {address}", learning.param), Instant::now())
        .map_err(|e| e.0)?;
    Ok((from.to_string(), used))
}

fn bind(param: &ParamPath, component: Option<u8>, from: &NodePath, channel: &str) -> Result<Op, String> {
    Ok(Op::Bind {
        param: param.clone(),
        component,
        from: from.clone(),
        channel: Some(Name::new(channel).map_err(|e| e.0)?),
    })
}

// ---------------------------------------------------------------------------------------------
// Commands

type CmdResult<T> = std::result::Result<T, CommandError>;

pub fn register(registry: &mut Registry, engine: &Arc<Engine>) {
    let mut add = |id: &'static str, summary: &'static str, f: fn(&Engine, &Json) -> CmdResult<Json>| {
        let engine = Arc::clone(engine);
        registry.register(FnCommand::new(id, summary, move |p| f(&engine, p)));
    };
    add(
        "osc.status",
        "OSC listeners (port, bound, packets, drops, sources), senders, recent messages, params set by address, learn",
        osc_status,
    );
    add(
        "osc.config",
        "OSC settings for this run: {enabled?, max_packet?, rate?, burst?, pattern_rate?, max_addresses?}",
        osc_config,
    );
    add(
        "osc.learn",
        "Bind a param to the next OSC address that arrives with a number; {param, timeout?, wait?} or {cancel: true}",
        osc_learn,
    );
    add(
        "osc.send",
        "Send one OSC message; {host?, port, address, args?: [numbers, strings, booleans]}",
        osc_send,
    );
}

fn osc_status(e: &Engine, _p: &Json) -> CmdResult<Json> {
    let osc = e.osc();
    let mut status = osc.hub().status();
    // Senders are labelled with their node id; show the path.
    let net = e.session().doc().snapshot();
    for s in &mut status.senders {
        let path = s
            .label
            .rsplit_once('#')
            .and_then(|(_, id)| id.parse::<u64>().ok())
            .and_then(|id| net.path_of(NodeId(id)));
        if let Some(p) = path {
            s.label = p.to_string();
        }
    }
    Ok(json!({
        "enabled": status.enabled,
        "listeners": status.listeners,
        "senders": status.senders,
        "recent": status.recent,
        "queued": status.queued,
        "params": osc.stats(),
        "learning": osc.learning(),
        "learned": osc.learned(),
        "local_ip": hub::local_ip().map(|ip| ip.to_string()),
        "config": osc.hub().config(),
    }))
}

fn osc_config(e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        enabled: Option<bool>,
        max_packet: Option<usize>,
        rate: Option<f64>,
        burst: Option<f64>,
        pattern_rate: Option<f64>,
        max_addresses: Option<usize>,
    }
    let a: A = args(p)?;
    let hub = e.osc().hub();
    let mut c: Config = hub.config();
    if let Some(v) = a.enabled {
        c.enabled = v;
    }
    if let Some(v) = a.max_packet {
        c.max_packet = v;
    }
    if let Some(v) = a.rate {
        c.rate = v;
    }
    if let Some(v) = a.burst {
        c.burst = v;
    }
    if let Some(v) = a.pattern_rate {
        c.pattern_rate = v;
    }
    if let Some(v) = a.max_addresses {
        c.max_addresses = v;
    }
    hub.set_config(c);
    Ok(serde_json::to_value(hub.config()).map_err(|e| e.to_string())?)
}

fn osc_learn(e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        param: Option<String>,
        timeout: Option<f64>,
        /// Seconds to wait for the outcome (at most the timeout).
        wait: Option<f64>,
        #[serde(default)]
        cancel: bool,
    }
    let a: A = args(p)?;
    let osc = e.osc();
    if a.cancel {
        return Ok(json!({"cancelled": osc.cancel_learn()}));
    }
    let param = a.param.ok_or("`param` is missing (or say {cancel: true})")?;
    let secs = |x: Option<f64>, d: f64| x.filter(|x| x.is_finite()).unwrap_or(d).clamp(0.0, 600.0);
    let timeout = Duration::from_secs_f64(secs(a.timeout, LEARN_TIMEOUT.as_secs_f64()).max(0.1));
    let before = osc.learned().map(|l| format!("{l:?}"));
    osc.learn(&e.session(), &param, timeout)?;
    let wait = Duration::from_secs_f64(secs(a.wait, 0.0)).min(timeout + Duration::from_millis(200));
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if osc.learning().is_none() && osc.learned().map(|l| format!("{l:?}")) != before {
            let l = osc.learned();
            if let Some(err) = l.as_ref().and_then(|l| l.error.clone()) {
                return Err(err.into());
            }
            return Ok(json!({"learned": l}));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(json!({"learning": param, "timeout": timeout.as_secs_f64()}))
}

fn json_arg(v: &Json) -> CmdResult<Arg> {
    Ok(match v {
        Json::Bool(true) => Arg::True,
        Json::Bool(false) => Arg::False,
        Json::Null => Arg::Nil,
        Json::String(s) => Arg::Str(s.clone()),
        Json::Number(n) => match n.as_i64() {
            Some(i) => i32::try_from(i).map(Arg::Int).unwrap_or(Arg::Long(i)),
            // `as`: OSC floats are 32 bits.
            None => Arg::Float(n.as_f64().unwrap_or(0.0) as f32),
        },
        _ => return Err("args are numbers, strings, booleans or null".into()),
    })
}

fn osc_send(e: &Engine, p: &Json) -> CmdResult<Json> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct A {
        #[serde(default = "localhost")]
        host: String,
        port: u16,
        address: String,
        #[serde(default)]
        args: Vec<Json>,
    }
    fn localhost() -> String {
        "127.0.0.1".into()
    }
    let a: A = args(p)?;
    if !a.address.starts_with('/') || a.address.contains(char::is_whitespace) {
        return Err("an address starts with / and has no spaces".into());
    }
    let target = hub::target(&a.host, a.port)?;
    let args = a.args.iter().map(json_arg).collect::<CmdResult<Vec<_>>>()?;
    let msg = Message::new(a.address, args);
    let packet = codec::message_packet(&msg);
    let sender = e.osc().hub().sender("osc.send", target).map_err(|e| e.to_string())?;
    sender.send(&packet, 1).map_err(|e| format!("sending to {target}: {e}"))?;
    Ok(json!({"sent": packet.len(), "to": target.to_string()}))
}
