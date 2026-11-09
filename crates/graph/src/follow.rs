//! A param that follows a param of a component ([`crate::Bind::param`]): an inner node's
//! `preset` bound to `parent.preset` takes the component's value whole, as it is now.
//!
//! Only a component's own params (its `@param` lines, [`crate::PARAMS`]) can be followed. An
//! exposed param (an `@expose` line) is an alias whose value lives in the inner node; following
//! it would mean following that inner node, so the binding is refused with a hint.
//!
//! What a follower takes, by the kind of its param:
//!
//! | follower      | takes                                                              |
//! |---------------|--------------------------------------------------------------------|
//! | bool, int, float | a bool, int or float (a bool counts 0 or 1; ints round)         |
//! | one component of a vector or colour | a bool, int or float                         |
//! | vector        | a vector of as many numbers                                        |
//! | colour        | a colour, or a vector of 3 or 4 numbers                            |
//! | choice        | a choice or a text, by name; a bool, int or float, by place        |
//! | text          | a choice or a text as it is; a bool, int or float written as text  |
//! | palette       | a palette, a choice or a text by name; a bool, int or float by place |
//!
//! Every other pair is refused when it is bound. File paths, node references and programs are
//! never driven by a binding ([`ParamSpec::bind_target`]).

use std::borrow::Cow;

use penumbra_core::bind as bindv;
use penumbra_core::param::MAX_TEXT;
use penumbra_core::{Error, Name, NodeId, NodePath, ParamKind, ParamSpec, Value};

use crate::bound::Resolved;
use crate::network::Network;
use crate::node::NodeTypes;

/// The word for the component a node sits in, in a binding source: `parent.preset` is param
/// `preset` of that component. It reaches one level up.
pub const PARENT: &str = "parent";

/// The spec of param `name` of `src` (at `from`) as a binding source: one of its `@param`
/// lines. Refused with a sentence for anything else.
pub fn source_spec<'t>(
    net: &Network,
    types: &'t NodeTypes,
    src: NodeId,
    from: &NodePath,
    name: &Name,
) -> Result<Cow<'t, ParamSpec>, Error> {
    let node = net.node(src).ok_or_else(|| Error::new(format!("no node at {from}")))?;
    let desc =
        types.desc(&node.kind).ok_or_else(|| Error::new(format!("unknown node kind `{}`", node.kind)))?;
    let Some(declared) = node.declared(desc) else {
        return Err(Error::new(format!(
            "{from} is a {}; a param can follow only a component's own params (its @param lines)",
            desc.kind
        )));
    };
    if let Some(spec) = declared.param(name.as_str()) {
        return Ok(Cow::Owned(spec.clone()));
    }
    if let Some(line) = declared.exposed().find(|l| l.name == *name) {
        return Err(Error::new(format!(
            "`{name}` on {from} shows the inner param {} (an @expose line). Bind to that param \
             directly, or declare `{name}` with an @param line and let {} follow it",
            line.target, line.target
        )));
    }
    Err(no_such(from, name, declared.params().map(|s| s.name.as_str()).collect()))
}

/// The refusal for a param `name` that component `from` does not declare.
pub fn no_such(from: &NodePath, name: &Name, params: Vec<&str>) -> Error {
    let has = if params.is_empty() {
        "it declares none (an @param line in its `params` adds one)".to_string()
    } else {
        format!("its params: {}", params.join(", "))
    };
    Error::new(format!("{from} has no param `{name}`; {has}"))
}

fn numeric(kind: &ParamKind) -> bool {
    matches!(kind, ParamKind::Bool | ParamKind::Int | ParamKind::Float)
}

fn wordy(kind: &ParamKind) -> bool {
    matches!(kind, ParamKind::Choice(_) | ParamKind::Text)
}

/// Whether `target` (component `component` of it, or the whole) may follow a param of spec
/// `source` (the table in the module).
pub fn check(target: &ParamSpec, component: Option<u8>, source: &ParamSpec) -> Result<(), Error> {
    // Files, node references and programs are never bound.
    if let Err(e) = target.bind_target(None)
        && component.is_none()
        && !matches!(target.kind, ParamKind::Vec(_) | ParamKind::Color)
    {
        return Err(e);
    }
    let s = &source.kind;
    let ok = match (&target.kind, component) {
        (_, Some(_)) => {
            target.check_bindable(component)?;
            numeric(s)
        }
        (ParamKind::Bool | ParamKind::Int | ParamKind::Float, None) => numeric(s),
        (ParamKind::Vec(n), None) => {
            matches!(s, ParamKind::Vec(m) if m == n) || (*n == 4 && matches!(s, ParamKind::Color))
        }
        (ParamKind::Color, None) => matches!(s, ParamKind::Color | ParamKind::Vec(3) | ParamKind::Vec(4)),
        (ParamKind::Choice(_) | ParamKind::Text, None) => numeric(s) || wordy(s),
        (ParamKind::Palette, None) => numeric(s) || wordy(s) || matches!(s, ParamKind::Palette),
        (ParamKind::File { .. }, None) => false,
    };
    if ok {
        return Ok(());
    }
    let part = match component {
        Some(c) => format!("{}.{c}", target.name),
        None => target.name.clone(),
    };
    let what = if component.is_some() { "a number".to_string() } else { target.kind_label() };
    Err(Error::new(format!(
        "`{part}` is {what} and cannot follow `{}`, which is {}",
        source.name,
        source.kind_label()
    )))
}

/// The problems of the params that follow component `comp`'s params, as their nodes reported
/// them at their last cook ([`crate::NodeStatus::follow_warnings`]): each follower's node and
/// one warning, ordered by node.
pub fn problems(
    net: &Network,
    status: &std::collections::BTreeMap<NodeId, crate::NodeStatus>,
    comp: NodeId,
) -> Vec<(NodeId, String)> {
    let mut nodes: Vec<NodeId> = net.followers(comp).into_values().flatten().map(|(id, _)| id).collect();
    nodes.sort_unstable();
    nodes.dedup();
    nodes
        .into_iter()
        .flat_map(|id| {
            let warnings = status.get(&id).map(|s| s.follow_warnings.clone()).unwrap_or_default();
            warnings.into_iter().map(move |w| (id, w))
        })
        .collect()
}

/// Whether `target` may follow `source` ([`check`]) and both are of a like kind: a bool, int or
/// float with one of its own kind, a choice or text with a choice or text, a vector with a vector of its size, a
/// colour or palette with its own kind. What the panel offers to follow, and what putting a
/// param on a component follows instead of declaring: a choice picked by a number's place, or
/// a float rounded into an int, is allowed by [`check`], but seldom meant.
pub fn alike(target: &ParamSpec, source: &ParamSpec) -> bool {
    let like = match (&target.kind, &source.kind) {
        (ParamKind::Bool, ParamKind::Bool) | (ParamKind::Int, ParamKind::Int) => true,
        (ParamKind::Float, ParamKind::Float) => true,
        (a, b) if wordy(a) && wordy(b) => true,
        (ParamKind::Palette, ParamKind::Palette | ParamKind::Choice(_) | ParamKind::Text) => true,
        (ParamKind::Vec(n), ParamKind::Vec(m)) => n == m,
        (ParamKind::Color, ParamKind::Color) => true,
        _ => false,
    };
    like && check(target, None, source).is_ok()
}

/// What a follower takes from the value it follows now.
pub(crate) enum Follow<'a> {
    /// The param (or the component of it) comes out as this value.
    Value(Value),
    /// The param comes out as this ([`Resolved`]).
    Resolved(Resolved<'a>),
    /// The value cannot be taken: the stored value applies, and the follower warns.
    Refused(String),
    /// A choice that lacks the entry named: it keeps its last entry, and the follower warns.
    Kept(String),
}

/// A bool, int or float as a number.
fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Int(i) => Some(*i as f64),
        Value::Float(x) => Some(*x),
        _ => None,
    }
}

/// What param `spec` (component `component` of it, or the whole) takes from `v`, the value it
/// follows; `current` is its value so far, which a component goes into.
pub(crate) fn follow<'a>(
    spec: &'a ParamSpec,
    component: Option<u8>,
    current: &Value,
    v: &'a Value,
) -> Follow<'a> {
    let refused = || {
        Follow::Refused(format!(
            "it follows {} {v}, and it is {}",
            v.type_name(),
            if component.is_some() { "one number".to_string() } else { spec.kind_label() }
        ))
    };
    let by_number = |x: f64| match spec.bind_value(current, component, x) {
        Ok(v) => Follow::Value(v),
        Err(e) => Follow::Refused(e.0),
    };
    if component.is_some() {
        return number(v).map_or_else(refused, by_number);
    }
    match &spec.kind {
        ParamKind::Bool | ParamKind::Int | ParamKind::Float => number(v).map_or_else(refused, by_number),
        ParamKind::Vec(_) | ParamKind::Color => match v {
            Value::Vec2(_) | Value::Vec3(_) | Value::Vec4(_) => match spec.check(v) {
                Ok(v) => Follow::Value(v),
                Err(_) => refused(),
            },
            _ => refused(),
        },
        ParamKind::Choice(choices) => match v {
            Value::Text(t) => match bindv::choice_named(choices, t) {
                Some(e) => Follow::Resolved(Resolved::Text(e)),
                None => Follow::Kept(format!(
                    "it follows \"{}\", which is none of {}; it keeps its last entry",
                    crate::bound::short(t),
                    choices.join(", ")
                )),
            },
            _ => match number(v) {
                Some(x) => match bindv::choice_at(choices, x) {
                    Some(e) => Follow::Resolved(Resolved::Text(e)),
                    None => Follow::Resolved(Resolved::Stored),
                },
                None => refused(),
            },
        },
        ParamKind::Text if spec.node_ref || spec.code => refused(),
        ParamKind::Text => match v {
            Value::Text(t) if t.len() > MAX_TEXT => refused(),
            Value::Text(t) => Follow::Resolved(Resolved::Text(t)),
            _ => match number(v) {
                Some(x) => Follow::Resolved(Resolved::Formatted(x, &bindv::DEFAULT_FORMAT)),
                None => refused(),
            },
        },
        // The source checked its palette when it was set.
        ParamKind::Palette => match v {
            Value::Text(t) if t.len() > penumbra_core::palette::MAX_VALUE => refused(),
            Value::Text(t) => Follow::Resolved(Resolved::Text(t)),
            _ => number(v).map_or_else(refused, by_number),
        },
        ParamKind::File { .. } => refused(),
    }
}

/// The value param `spec` takes from `v` as [`follow`] gives it, as a plain value to store
/// (dissolving a component turns its followers into values); `None` when it takes nothing.
pub fn value(spec: &ParamSpec, component: Option<u8>, current: &Value, v: &Value) -> Option<Value> {
    match follow(spec, component, current, v) {
        Follow::Value(v) => Some(v),
        Follow::Resolved(Resolved::Text(t)) => Some(Value::Text(t.to_string())),
        Follow::Resolved(Resolved::Formatted(x, f)) => Some(Value::Text(f.to_text(x))),
        Follow::Resolved(Resolved::Value(v)) => Some(v),
        Follow::Resolved(Resolved::Keep | Resolved::Stored) | Follow::Refused(_) | Follow::Kept(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str, kind: ParamKind, default: Value) -> ParamSpec {
        let mut s = ParamSpec::float(name, 0.0);
        s.kind = kind;
        s.default = default;
        s
    }

    fn choice(entries: &[&str]) -> ParamSpec {
        let list: Vec<String> = entries.iter().map(|s| s.to_string()).collect();
        spec("preset", ParamKind::Choice(list), Value::Text(entries[0].into()))
    }

    #[test]
    fn the_matrix_takes_like_kinds_and_refuses_the_rest() {
        let float = ParamSpec::float("x", 0.0);
        let int = spec("n", ParamKind::Int, Value::Int(0));
        let text = spec("t", ParamKind::Text, Value::Text(String::new()));
        let v3 = spec("v", ParamKind::Vec(3), Value::Vec3([0.0; 3]));
        let colour = spec("c", ParamKind::Color, Value::Vec4([0.0, 0.0, 0.0, 1.0]));
        let pick = choice(&["a", "b"]);
        assert!(check(&float, None, &int).is_ok());
        assert!(check(&int, None, &float).is_ok());
        assert!(check(&pick, None, &choice(&["b", "c"])).is_ok());
        assert!(check(&pick, None, &int).is_ok());
        assert!(check(&text, None, &pick).is_ok());
        assert!(check(&text, None, &float).is_ok());
        assert!(check(&colour, None, &v3).is_ok());
        assert!(check(&v3, Some(1), &float).is_ok());
        let e = check(&float, None, &pick).unwrap_err().0;
        assert_eq!(e, "`x` is a number and cannot follow `preset`, which is a choice");
        assert!(check(&v3, None, &colour).is_err());
        assert!(check(&v3, None, &float).is_err());
        assert!(check(&v3, Some(1), &v3).is_err());
        assert!(check(&float, Some(0), &float).unwrap_err().0.contains("as a whole"));
    }

    #[test]
    fn values_pass_whole_and_choices_by_name() {
        let pick = choice(&["worms", "coral"]);
        let v = Value::Text("coral".into());
        assert!(matches!(follow(&pick, None, &pick.default, &v), Follow::Resolved(Resolved::Text("coral"))));
        let gone = Value::Text("maze".into());
        let Follow::Kept(w) = follow(&pick, None, &pick.default, &gone) else { panic!() };
        assert_eq!(w, "it follows \"maze\", which is none of worms, coral; it keeps its last entry");
        let int = spec("n", ParamKind::Int, Value::Int(0));
        assert_eq!(value(&int, None, &int.default, &Value::Float(2.6)), Some(Value::Int(3)));
        let colour = spec("c", ParamKind::Color, Value::Vec4([0.0; 4]));
        assert_eq!(
            value(&colour, None, &colour.default, &Value::Vec3([0.5, 0.25, 1.0])),
            Some(Value::Vec4([0.5, 0.25, 1.0, 1.0]))
        );
        let v3 = spec("v", ParamKind::Vec(3), Value::Vec3([0.0; 3]));
        assert_eq!(value(&v3, Some(2), &v3.default, &Value::Bool(true)), Some(Value::Vec3([0.0, 0.0, 1.0])));
        let text = spec("t", ParamKind::Text, Value::Text(String::new()));
        assert_eq!(value(&text, None, &text.default, &Value::Float(0.5)), Some(Value::Text("0.5".into())));
        assert!(matches!(follow(&v3, None, &v3.default, &Value::Float(1.0)), Follow::Refused(_)));
    }
}
