//! Presets of a component: its `@preset` lines ([`penumbra_core::declare::Presets`]) say which
//! values choosing an entry of a choice writes into its params.
//!
//! The write happens in the edit itself, for interactive edits only ([`crate::Params::Strict`]):
//! a set of a choice with presets is followed, in the same batch, by sets of the values its
//! chosen entry gives (the `*` base, then the entry's own). So choosing a preset is one undo
//! step, and the values are ordinary stored values afterwards, tuned and saved like any other.
//! Loading or merging a project file never writes: a hand-edited value stays as the file says.
//! A choice whose presets another choice's presets set (`outside` under `preset`) writes its own
//! values too, one level deep.
//!
//! A reset of a param the presets own goes to the chosen entry's value, not to the declared
//! default: the preset is what the component "is" until another entry is chosen.

use penumbra_core::declare::{Declared, Presets};
use penumbra_core::{Error, Name, NodeId, NodePath, ParamPath, Value};

use crate::network::{Applied, Network, Op};
use crate::node::NodeTypes;

/// Where a write of presets stands, for its reports.
struct Writes {
    ops: Vec<Op>,
    /// `choice=entry`, for the report of a value that does not go in.
    from: String,
}

/// What a component's presets say now, for `node.info` and the panel.
#[derive(Clone, Debug, PartialEq)]
pub struct PresetStatus {
    /// The `@param` choice.
    pub choice: Name,
    /// Every entry of the choice in its order, each with the docs of its lines.
    pub entries: Vec<(String, String)>,
    /// The params choosing an entry writes (the `*` lines).
    pub owns: Vec<Name>,
    /// The entry in effect: the stored one, else the declared default.
    pub chosen: String,
    /// The owned params whose value differs from the chosen entry's.
    pub changed: Vec<Name>,
    /// Values that would not go in now: an `@expose` line that names no inner param, or a
    /// value the inner param refuses. Choosing the entry skips them.
    pub problems: Vec<String>,
    /// The owned params that are bound (on the component, or inside where an `@expose` line
    /// shows them): choosing an entry writes their stored value, and the binding decides the
    /// value in effect.
    pub bound: Vec<Name>,
}

/// Whether two values are the same for presets: numbers by value (`1` and `1.0`).
fn same(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

impl Network {
    /// Component `id` with its kind's declarations, when it declares presets.
    fn with_presets(&self, types: &NodeTypes, id: NodeId) -> Option<(NodePath, Declared)> {
        let declared = self.declared(types, id)?;
        if declared.presets.is_empty() {
            return None;
        }
        Some((self.path_of(id)?, declared))
    }

    /// The entry of `presets` in effect on component `id`: its stored choice, else the declared
    /// default.
    fn chosen(&self, types: &NodeTypes, id: NodeId, presets: &Presets) -> Option<String> {
        let node = self.node(id)?;
        let value = node.value(types.desc(&node.kind)?, presets.choice.as_str())?;
        value.as_str().map(str::to_string)
    }

    /// The sets that choosing the entry now in effect of choice `param` writes, when `param` is
    /// a choice with presets.
    fn preset_writes(&self, types: &NodeTypes, param: &ParamPath) -> Option<Writes> {
        let id = self.resolve(param.node())?;
        let (path, declared) = self.with_presets(types, id)?;
        let presets = declared.presets_of(param.param().as_str())?;
        let entry = self.chosen(types, id, presets)?;
        let ops = presets
            .values(&entry)
            .into_iter()
            .filter_map(|(name, value)| Some(Op::Set { param: path.param(name).ok()?, value }))
            .collect();
        Some(Writes { ops, from: format!("{}={entry}", presets.choice) })
    }

    /// The value a reset of `param` goes to when a component's presets own it: the chosen
    /// entry's, as a set on the component. `param` may name the component's param or the inner
    /// param an `@expose` line of an enclosing component shows.
    pub fn preset_reset(&self, types: &NodeTypes, param: &ParamPath) -> Option<Op> {
        let id = self.resolve(param.node())?;
        let owned = |comp: NodeId, name: &str| -> Option<Op> {
            let (path, declared) = self.with_presets(types, comp)?;
            let presets = declared.owner(name)?;
            let entry = self.chosen(types, comp, presets)?;
            let value = presets.value(&entry, name)?.clone();
            Some(Op::Set { param: path.param(Name::new(name).ok()?).ok()?, value })
        };
        if let Some(op) = owned(id, param.param().as_str()) {
            return Some(op);
        }
        // An inner param shown on a component around it.
        let mut up = self.node(id)?.parent;
        for _ in 0..=NodePath::MAX_DEPTH {
            let comp = up?;
            if let Some((path, declared)) = self.with_presets(types, comp) {
                let shown = crate::exposed_of(&path, &declared)
                    .into_iter()
                    .find(|x| x.target.as_ref() == Ok(param) && declared.owner(x.name.as_str()).is_some());
                if let Some(x) = shown {
                    return owned(comp, x.name.as_str());
                }
            }
            up = self.node(comp)?.parent;
        }
        None
    }

    /// What the presets of component `id` say now ([`PresetStatus`]). `read` gives the value
    /// of an inner param an `@expose` line shows, when the network alone does not know it (a
    /// shader param at its default); the stored value is asked first.
    pub fn preset_status(
        &self,
        types: &NodeTypes,
        id: NodeId,
        read: impl Fn(&ParamPath) -> Option<Value>,
    ) -> Vec<PresetStatus> {
        let Some((path, declared)) = self.with_presets(types, id) else { return Vec::new() };
        let Some(node) = self.node(id) else { return Vec::new() };
        let Some(desc) = types.desc(&node.kind) else { return Vec::new() };
        // The value in effect of an owned param, and the spec of the inner param an exposed
        // one stands for.
        let current = |name: &Name| -> (Option<Value>, Result<Option<ParamPath>, String>) {
            if declared.param(name.as_str()).is_some() {
                return (node.value(desc, name.as_str()), Ok(None));
            }
            let target = path.param(name.clone()).and_then(|p| self.resolve_alias(types, &p));
            match target {
                Ok(t) if self.resolve(t.node()).is_none() => (None, Err(format!("no node at {}", t.node()))),
                Ok(t) => {
                    // Stored, else the default of the inner node's kind or declaration, else
                    // what `read` knows (a shader's header).
                    let stored = self
                        .resolve(t.node())
                        .and_then(|n| self.node(n))
                        .and_then(|n| n.value(types.desc(&n.kind)?, t.param().as_str()));
                    (stored.or_else(|| read(&t)), Ok(Some(t)))
                }
                Err(e) => (None, Err(e.0)),
            }
        };
        declared
            .presets
            .iter()
            .map(|presets| {
                let entries: Vec<String> = match declared.param(presets.choice.as_str()).map(|s| &s.kind) {
                    Some(penumbra_core::ParamKind::Choice(list)) => list.clone(),
                    _ => Vec::new(),
                };
                let chosen = self.chosen(types, id, presets).unwrap_or_default();
                let mut changed = Vec::new();
                let mut problems = Vec::new();
                let mut bound = Vec::new();
                for (name, _) in &presets.base {
                    let (now, target) = current(name);
                    let target = match target {
                        Ok(t) => t,
                        Err(e) => {
                            problems.push(format!("`{name}`: {e}"));
                            continue;
                        }
                    };
                    let is_bound = match &target {
                        None => node.is_bound(name.as_str()),
                        Some(t) => self
                            .resolve(t.node())
                            .and_then(|n| self.node(n))
                            .is_some_and(|n| n.is_bound(t.param().as_str())),
                    };
                    if is_bound {
                        bound.push(name.clone());
                    }
                    if let Some(t) = &target
                        && let Some(spec) =
                            self.resolve(t.node()).and_then(|n| self.spec_of(types, n, t.param().as_str()))
                    {
                        for entry in entries.iter().map(String::as_str) {
                            if let Some(Err(e)) = presets.value(entry, name.as_str()).map(|v| spec.check(v)) {
                                problems.push(format!("`{name}` of {entry}: {e}"));
                            }
                        }
                    }
                    let wanted = presets.value(&chosen, name.as_str());
                    if let (Some(now), Some(wanted)) = (now, wanted)
                        && !same(&now, wanted)
                    {
                        changed.push(name.clone());
                    }
                }
                problems.dedup();
                PresetStatus {
                    choice: presets.choice.clone(),
                    entries: entries
                        .iter()
                        .map(|e| (e.clone(), presets.entry(e).map(|x| x.doc.clone()).unwrap_or_default()))
                        .collect(),
                    owns: presets.base.iter().map(|(n, _)| n.clone()).collect(),
                    chosen,
                    changed,
                    problems,
                    bound,
                }
            })
            .collect()
    }
}

impl Applied {
    /// Applies `op` as an interactive edit does: a reset of a param presets own goes to the
    /// chosen entry's value ([`Network::preset_reset`]), and a set of a choice with presets
    /// writes its entry's values after it. A value that does not go in is skipped and named in
    /// [`Applied::left_out`]; only `op` itself can fail the batch.
    pub(crate) fn apply_interactive(&mut self, types: &NodeTypes, op: &Op) -> Result<(), Error> {
        let reset = match op {
            Op::Reset { param } => self.network.preset_reset(types, param),
            _ => None,
        };
        match reset {
            Some(set) => {
                if let Err(e) = self.apply_one(types, &set) {
                    // The chosen entry's value does not go in: reset as without presets.
                    self.left_out.push(format!("{}: {e}", set.label()));
                    self.apply_one(types, op)?;
                    return Ok(());
                }
                self.write_presets(types, &set, 0);
            }
            None => {
                self.apply_one(types, op)?;
                self.write_presets(types, op, 0);
            }
        }
        Ok(())
    }

    /// After `op` went in: when it set (or reset) a choice with presets, the values its chosen
    /// entry gives. `depth` 0 is the choice the edit named; a choice among the values writes
    /// its own presets at depth 1, and no deeper.
    fn write_presets(&mut self, types: &NodeTypes, op: &Op, depth: u8) {
        let param = match op {
            Op::Set { param, .. } | Op::Reset { param } => param,
            _ => return,
        };
        let Some(writes) = self.network.preset_writes(types, param) else { return };
        for set in &writes.ops {
            match self.apply_one(types, set) {
                Ok(()) if depth == 0 => self.write_presets(types, set, 1),
                Ok(()) => {}
                Err(e) => {
                    let why = format!("{} (preset {}): {e}", set.label(), writes.from);
                    log::warn!("{why}");
                    self.left_out.push(why);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Params;
    use crate::testing::{create, test_types};

    fn pp(s: &str) -> ParamPath {
        ParamPath::parse(s).unwrap()
    }

    fn set(param: &str, value: impl Into<Value>) -> Op {
        Op::Set { param: pp(param), value: value.into() }
    }

    const TEXT: &str = "@param preset choice(coral|maze|letters) = coral\n\
                        @param feed float = 0.05\n\
                        @param kill float = 0.06\n\
                        @param speed float = 1\n\
                        @param outside choice(off|coral)\n\
                        @param outside_feed float\n\
                        @expose level = k.value\n\
                        @preset preset=* feed=0.0545 kill=0.062 level=2 outside=off\n\
                        @preset preset=maze feed=0.029 kill=0.057 level=3\n\
                        @preset preset=letters outside=coral\n\
                        @preset outside=* outside_feed=0\n\
                        @preset outside=coral outside_feed=0.07";

    fn net() -> Network {
        let types = test_types();
        Network::new()
            .apply(&types, &[create("/c", "test.comp"), create("/c/k", "test.const"), set("/c.params", TEXT)])
            .unwrap()
            .network
    }

    fn value(net: &Network, param: &str) -> Option<Value> {
        let p = pp(param);
        let id = net.resolve(p.node())?;
        let node = net.node(id)?;
        node.value(test_types().desc(&node.kind)?, p.param().as_str())
    }

    fn num(net: &Network, param: &str) -> f64 {
        value(net, param).and_then(|v| v.as_f64()).unwrap_or(f64::NAN)
    }

    #[test]
    fn choosing_an_entry_writes_the_base_then_its_own_values_in_the_same_edit() {
        let types = test_types();
        let net = net();
        let n = net.apply(&types, &[set("/c.preset", "maze")]).unwrap();
        assert!(n.left_out.is_empty(), "{:?}", n.left_out);
        let n = n.network;
        assert_eq!((num(&n, "/c.feed"), num(&n, "/c.kill")), (0.029, 0.057));
        assert_eq!(num(&n, "/c/k.value"), 3.0, "an exposed name writes the inner param");
        assert_eq!(num(&n, "/c.speed"), 1.0, "a param on no `*` line stays the user's");
        // Tuned, then another entry: the base comes back where the entry says nothing.
        let n = n
            .apply(&types, &[set("/c.speed", 2.0), set("/c.feed", 0.04), set("/c.preset", "coral")])
            .unwrap();
        let n = n.network;
        assert_eq!((num(&n, "/c.feed"), num(&n, "/c.kill"), num(&n, "/c/k.value")), (0.0545, 0.062, 2.0));
        assert_eq!(num(&n, "/c.speed"), 2.0);
        // The chosen entry again: its values again.
        let tuned = n.apply(&types, &[set("/c.feed", 0.01)]).unwrap().network;
        let again = tuned.apply(&types, &[set("/c.preset", "coral")]).unwrap().network;
        assert_eq!(num(&again, "/c.feed"), 0.0545);
        // A set after the choice in the same batch wins; one before it is overwritten.
        let n = net.apply(&types, &[set("/c.preset", "maze"), set("/c.feed", 0.03)]).unwrap().network;
        assert_eq!(num(&n, "/c.feed"), 0.03);
        let n = net.apply(&types, &[set("/c.feed", 0.03), set("/c.preset", "maze")]).unwrap().network;
        assert_eq!(num(&n, "/c.feed"), 0.029);
    }

    #[test]
    fn a_choice_among_the_values_writes_its_own_presets_one_level_deep() {
        let types = test_types();
        let n = net().apply(&types, &[set("/c.preset", "letters")]).unwrap().network;
        assert_eq!(value(&n, "/c.outside"), Some(Value::Text("coral".into())));
        assert_eq!(num(&n, "/c.outside_feed"), 0.07);
        let n = n.apply(&types, &[set("/c.outside", "off")]).unwrap().network;
        assert_eq!(num(&n, "/c.outside_feed"), 0.0);
        assert_eq!(value(&n, "/c.preset"), Some(Value::Text("letters".into())), "the main choice stays");
        let n = n.apply(&types, &[set("/c.preset", "maze")]).unwrap().network;
        assert_eq!(
            (value(&n, "/c.outside"), num(&n, "/c.outside_feed")),
            (Some(Value::Text("off".into())), 0.0)
        );
    }

    #[test]
    fn loading_a_file_never_writes() {
        let types = test_types();
        let ops = [set("/c.preset", "maze"), set("/c.feed", 0.04)];
        let n = net().apply_with(&types, &ops, Params::Keep).unwrap().network;
        assert_eq!(num(&n, "/c.feed"), 0.04);
        assert_eq!(num(&n, "/c.kill"), 0.06, "the declared default, not the preset's value");
        let n = net()
            .apply_with(&types, &[set("/c.feed", 0.04), set("/c.preset", "maze")], Params::Keep)
            .unwrap();
        assert_eq!(num(&n.network, "/c.feed"), 0.04, "in any order");
    }

    #[test]
    fn a_reset_of_an_owned_param_goes_to_the_chosen_entry() {
        let types = test_types();
        let n = net()
            .apply(&types, &[set("/c.preset", "maze"), set("/c.feed", 0.01), set("/c/k.value", 9.0)])
            .unwrap();
        let n = n.network;
        let back = n
            .apply(&types, &[Op::Reset { param: pp("/c.feed") }, Op::Reset { param: pp("/c/k.value") }])
            .unwrap()
            .network;
        assert_eq!(num(&back, "/c.feed"), 0.029, "maze's value, not the declared 0.05");
        assert_eq!(num(&back, "/c/k.value"), 3.0, "also for the inner param an @expose line shows");
        let speed = n.apply(&types, &[set("/c.speed", 3.0)]).unwrap().network;
        let speed = speed.apply(&types, &[Op::Reset { param: pp("/c.speed") }]).unwrap().network;
        assert_eq!(num(&speed, "/c.speed"), 1.0, "a param presets do not own goes to its default");
        // A reset of the choice chooses its default entry, with its values.
        let coral = n.apply(&types, &[Op::Reset { param: pp("/c.preset") }]).unwrap().network;
        assert_eq!((num(&coral, "/c.feed"), num(&coral, "/c/k.value")), (0.0545, 2.0));
    }

    #[test]
    fn a_value_that_does_not_go_in_is_skipped_and_named() {
        let types = test_types();
        // `level` shows a param of a node that is gone.
        let n = net().apply(&types, &[Op::Delete { path: "/c/k".parse().unwrap() }]).unwrap().network;
        let applied = n.apply(&types, &[set("/c.preset", "maze")]).unwrap();
        assert_eq!(num(&applied.network, "/c.feed"), 0.029, "the others go in");
        assert_eq!(applied.left_out.len(), 1, "{:?}", applied.left_out);
        assert!(applied.left_out[0].contains("/c.level") && applied.left_out[0].contains("preset=maze"));
        let c = n.resolve(&"/c".parse().unwrap()).unwrap();
        let status = n.preset_status(&types, c, |_| None);
        assert!(status[0].problems.iter().any(|p| p.starts_with("`level`")), "{:?}", status[0].problems);
    }

    #[test]
    fn the_status_names_the_chosen_entry_and_what_differs_from_it() {
        let types = test_types();
        let n = net().apply(&types, &[set("/c.preset", "maze")]).unwrap().network;
        let c = n.resolve(&"/c".parse().unwrap()).unwrap();
        let status = n.preset_status(&types, c, |_| None);
        assert_eq!(status.len(), 2);
        let main = &status[0];
        assert_eq!((main.choice.as_str(), main.chosen.as_str()), ("preset", "maze"));
        let owns: Vec<&str> = main.owns.iter().map(Name::as_str).collect();
        assert_eq!(owns, ["feed", "kill", "level", "outside"]);
        assert_eq!(main.entries.len(), 3);
        assert!(main.changed.is_empty(), "{:?}", main.changed);
        let n = n.apply(&types, &[set("/c.kill", 0.05), set("/c/k.value", 4.0)]).unwrap().network;
        let status = n.preset_status(&types, c, |_| None);
        let changed: Vec<&str> = status[0].changed.iter().map(Name::as_str).collect();
        assert_eq!(changed, ["kill", "level"]);
        assert_eq!(status[1].chosen, "off");
    }

    /// An inner param at the default of its kind counts as its value: a component saved
    /// without choosing its entry once shows it as changed.
    #[test]
    fn an_inner_param_at_its_default_counts_for_changed() {
        let types = test_types();
        let n = net();
        let c = n.resolve(&"/c".parse().unwrap()).unwrap();
        let status = n.preset_status(&types, c, |_| None);
        let changed: Vec<&str> = status[0].changed.iter().map(Name::as_str).collect();
        assert_eq!(changed, ["feed", "kill", "level"], "the declared defaults differ from the base");
    }
}
