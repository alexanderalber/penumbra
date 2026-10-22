//! Node types: what a node declares and how it cooks.

use std::any::Any;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use penumbra_core::{Error, Name, ParamSpec};
use serde::{Deserialize, Serialize};

use crate::cook::{CookContext, InputChoice};
use crate::job::JobStatus;
use crate::network::NodeDef;

/// The family of data flowing along a connection. An output connects only to an input of the same
/// kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataKind {
    Tex,
    Chan,
    Table,
    Geo,
    /// A material, put on geometry by `geo.assign_material`.
    Mat,
    /// Lights for a render.
    Light,
    /// One camera: where it is, where it looks, its lens (`view.camera`).
    View,
}

/// One cooked output. The payload type belongs to the crate that implements the family (a GPU
/// texture handle lives in `gpu`, which this crate does not know), so it is type-erased here;
/// cloning is an `Arc` clone.
#[derive(Clone)]
pub struct Data {
    kind: DataKind,
    value: Arc<dyn Any + Send + Sync>,
}

impl Data {
    pub fn new<T: Any + Send + Sync>(kind: DataKind, value: T) -> Self {
        Self { kind, value: Arc::new(value) }
    }

    pub fn kind(&self) -> DataKind {
        self.kind
    }

    pub fn get<T: Any>(&self) -> Option<&T> {
        self.value.downcast_ref()
    }

    /// What a lazy output ([`NodeDesc::lazy_output`]) holds while nothing reads it: of the
    /// declared kind, but with no value ([`Data::get`] gives `None`).
    pub fn skipped(kind: DataKind) -> Self {
        Self::new(kind, Skipped)
    }

    /// Whether this is a [`Data::skipped`] placeholder.
    pub fn is_skipped(&self) -> bool {
        self.value.is::<Skipped>()
    }

    /// Whether both are the same cooked result (not merely equal).
    pub fn same(&self, other: &Data) -> bool {
        Arc::ptr_eq(&self.value, &other.value)
    }

    /// The address of the cooked result, to tell later whether a node still holds the same one
    /// without keeping it alive. Equal for [`Data::same`] results; a result dropped meanwhile
    /// may share it with a new one.
    pub fn address(&self) -> usize {
        Arc::as_ptr(&self.value).cast::<()>() as usize
    }
}

/// The value of [`Data::skipped`].
struct Skipped;

impl fmt::Debug for Data {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Data({:?})", self.kind)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct InputSpec {
    pub name: String,
    pub kind: DataKind,
    pub doc: String,
    /// The input sees what its source held before this frame's cook: it is no dependency, the
    /// source may lie downstream, and a wire into it may close a loop (tex.feedback's
    /// `previous`).
    pub previous: bool,
}

impl InputSpec {
    pub fn new(name: &str, kind: DataKind) -> Self {
        Self { name: name.to_string(), kind, doc: String::new(), previous: false }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OutputSpec {
    pub name: String,
    pub kind: DataKind,
    /// What the output holds, for the reference and the editor's port tooltip; worth writing
    /// for every output of a node with several.
    pub doc: String,
    /// Made only while something reads it ([`NodeDesc::lazy_output`]).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub lazy: bool,
}

impl OutputSpec {
    pub fn new(name: &str, kind: DataKind) -> Self {
        Self { name: name.to_string(), kind, doc: String::new(), lazy: false }
    }
}

/// What a node does at the edge of a component: an input node is fed from outside the component and
/// shows as an input of it; an output node is read from outside and shows as an output of it. Both
/// pass their one input on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PortRole {
    In,
    Out,
}

/// Everything a node type declares about itself. Owned data rather than `&'static`, because
/// some node types (the shader node) derive their parameters from a file.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NodeDesc {
    /// `family.name`, e.g. `tex.noise`. This is what the project file stores.
    pub kind: String,
    pub summary: String,
    pub params: Vec<ParamSpec>,
    pub inputs: Vec<InputSpec>,
    /// Empty for sinks (`out` family).
    pub outputs: Vec<OutputSpec>,
    /// Reads the show clock, so it recooks whenever time moves. A node that reads the clock
    /// without declaring it gets a warning (see [`CookContext::time`]); dirty tracking depends on
    /// the declaration being right.
    pub time_dependent: bool,
    /// May hold child nodes (components, M2b).
    pub container: bool,
    /// An input or output of the component it sits in ([`PortRole`]).
    pub port: Option<PortRole>,
    /// Accepts parameters beyond `params`, declared per instance at cook time (the shader node
    /// reads them from its file, [`Operator::params`]). Such values are only checked for being
    /// finite when set; the operator checks them against its own declaration.
    pub extra_params: bool,
    /// Holds what earlier cooks made beyond its params: a feedback loop's frame, a cache's or a
    /// time machine's frames, a learned background, a running sum. `node.reset` starts such a
    /// node over ([`Cooker::reset`](crate::Cooker::reset)), and the editor offers a reset button.
    pub stateful: bool,
    /// Its first output's channels usually carry many samples each (a trail, an audio block):
    /// the editor gives it a taller body with a plot.
    pub sampled: bool,
}

impl NodeDesc {
    pub fn new(kind: &str, summary: &str) -> Self {
        Self {
            kind: kind.to_string(),
            summary: summary.to_string(),
            params: Vec::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            time_dependent: false,
            container: false,
            port: None,
            extra_params: false,
            stateful: false,
            sampled: false,
        }
    }

    pub fn param(mut self, spec: ParamSpec) -> Self {
        self.params.push(spec);
        self
    }

    pub fn input(mut self, name: &str, kind: DataKind) -> Self {
        self.inputs.push(InputSpec::new(name, kind));
        self
    }

    /// An input that reads its source's previous result ([`InputSpec::previous`]).
    pub fn input_previous(mut self, name: &str, kind: DataKind) -> Self {
        self.inputs.push(InputSpec { previous: true, ..InputSpec::new(name, kind) });
        self
    }

    pub fn output(mut self, name: &str, kind: DataKind) -> Self {
        self.outputs.push(OutputSpec::new(name, kind));
        self
    }

    /// Describes the output `name` declared before ([`OutputSpec::doc`]); unknown names are
    /// ignored here; a test in `penumbra-ops` checks that every output of a node with several is
    /// described.
    pub fn describe_output(mut self, name: &str, doc: &str) -> Self {
        if let Some(o) = self.outputs.iter_mut().find(|o| o.name == name) {
            o.doc = doc.to_string();
        }
        self
    }

    /// Marks the output `name` declared before as lazy: costly to make and often unused, so the
    /// node makes it only while something reads it ([`CookContext::output_wanted`]) and puts
    /// [`Data::skipped`] there otherwise. A change in whether it is read recooks the node.
    pub fn lazy_output(mut self, name: &str) -> Self {
        if let Some(o) = self.outputs.iter_mut().find(|o| o.name == name) {
            o.lazy = true;
        }
        self
    }

    /// Describes the input `name` declared before ([`InputSpec::doc`]).
    pub fn describe_input(mut self, name: &str, doc: &str) -> Self {
        if let Some(i) = self.inputs.iter_mut().find(|i| i.name == name) {
            i.doc = doc.to_string();
        }
        self
    }

    pub fn time_dependent(mut self) -> Self {
        self.time_dependent = true;
        self
    }

    pub fn container(mut self) -> Self {
        self.container = true;
        self
    }

    /// An input or output of its component: one input and one output of the same kind.
    pub fn port(mut self, role: PortRole) -> Self {
        self.port = Some(role);
        self
    }

    pub fn extra_params(mut self) -> Self {
        self.extra_params = true;
        self
    }

    /// Declares [`NodeDesc::stateful`].
    pub fn stateful(mut self) -> Self {
        self.stateful = true;
        self
    }

    /// Declares [`NodeDesc::sampled`].
    pub fn sampled(mut self) -> Self {
        self.sampled = true;
        self
    }

    pub fn param_spec(&self, name: &str) -> Option<&ParamSpec> {
        self.params.iter().find(|p| p.name == name)
    }

    /// Kind syntax, unique valid names, valid param specs.
    pub fn validate(&self) -> Result<(), Error> {
        let ctx = || format!("node type `{}`", self.kind);
        let Some((family, name)) = self.kind.split_once('.') else {
            return Err(Error::new("kind must be `family.name`").context(ctx()));
        };
        let lower = |s: &str| {
            s.chars().next().is_some_and(|c| c.is_ascii_lowercase())
                && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        };
        if !lower(family) || !lower(name) {
            return Err(Error::new("kind parts must be lower case letters, digits and `_`").context(ctx()));
        }
        let mut seen = Vec::new();
        for p in &self.params {
            p.validate_spec().map_err(|e| e.context(ctx()))?;
            if seen.contains(&p.name.as_str()) {
                return Err(Error::new(format!("param `{}` declared twice", p.name)).context(ctx()));
            }
            seen.push(&p.name);
        }
        if self.port.is_some() {
            let one = |n: usize| n == 1;
            let same =
                matches!((self.inputs.first(), self.outputs.first()), (Some(i), Some(o)) if i.kind == o.kind);
            if !(one(self.inputs.len()) && one(self.outputs.len()) && same && !self.container) {
                return Err(Error::new("a component port has one input and one output of the same kind")
                    .context(ctx()));
            }
        }
        for (what, names) in [
            ("input", self.inputs.iter().map(|i| i.name.as_str()).collect::<Vec<_>>()),
            ("output", self.outputs.iter().map(|o| o.name.as_str()).collect()),
        ] {
            for (i, n) in names.iter().enumerate() {
                Name::new(n).map_err(|e| e.context(format!("{what} {i}")).context(ctx()))?;
                if names.iter().take(i).any(|m| m == n) {
                    return Err(Error::new(format!("{what} `{n}` declared twice")).context(ctx()));
                }
            }
        }
        Ok(())
    }
}

/// What a successful cook produced.
pub enum Cooked {
    /// New outputs, one per declared output, in order.
    Outputs(Vec<Data>),
    /// The previous outputs still hold (e.g. an asynchronous node whose job has not finished);
    /// downstream nodes are not recooked.
    Unchanged,
}

/// One piece of a node's state kept across a restart: what [`Operator::save_state`] hands out and
/// [`Operator::restore_state`] gets back from the snapshot on disk.
#[derive(Clone, Debug)]
pub enum StateItem {
    /// Data as it flows between nodes. The host knows how to store textures, channels and
    /// tables; other kinds (geometry) are left out of the snapshot.
    Data(Data),
    /// Anything else, in the operator's own encoding (a tracker's ids and positions as JSON,
    /// say). Read back from disk, so the operator must treat it as hostile.
    Bytes(Vec<u8>),
}

/// Why a cook produced nothing new. Either way the last good outputs stay in place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CookError {
    /// No result yet; an asynchronous job is on its way. Not shown as an error.
    Pending,
    Failed(String),
}

impl CookError {
    pub fn failed(message: impl Into<String>) -> Self {
        Self::Failed(message.into())
    }
}

impl From<Error> for CookError {
    fn from(e: Error) -> Self {
        Self::Failed(e.0)
    }
}

/// Which inputs a node needs this frame, decided from its parameters (bound ones already
/// evaluated) and its wiring, before anything upstream of those inputs cooks: the indices of
/// the needed inputs, or `None` for all of them. Inputs left out are not cooked for this node
/// and read as nothing in its cook. Must be cheap and depend on nothing but what
/// [`InputChoice`] offers, since the cooker also asks it to judge whether a picture is current.
pub type InputChooser = fn(&InputChoice<'_>) -> Option<Vec<usize>>;

/// A registered node type: its declaration and a factory for its runtime state.
pub trait NodeType: Send + Sync {
    fn desc(&self) -> &NodeDesc;
    /// A fresh operator. Called when a node is created, when its kind changes and after an
    /// operator panicked.
    fn create(&self) -> Box<dyn Operator>;
    /// Picks the inputs a node of this type needs ([`InputChooser`]); `None`, the default,
    /// needs every input, always.
    fn input_chooser(&self) -> Option<InputChooser> {
        None
    }

    /// Prepares what the nodes of this type need before their first cook, off the cook thread
    /// ([`WarmUp`]). Does nothing by default.
    fn warm_up(&self, _resource: &dyn Any) -> Result<(), Error> {
        Ok(())
    }

    /// Why param `name` of `node` does not count with its other params as they are (the param
    /// panel greys it and says why); `None`, the default, while it counts.
    fn inactive(&self, _node: &NodeDef, _name: &str) -> Option<&'static str> {
        None
    }
}

/// A node type's preparation for the first cook, such as compiling its shaders, so that the
/// first node of the type does not stall the cook thread. Gets a resource of the app's (the
/// GPU); a hook that does not know the resource does nothing.
pub type WarmUp = Arc<dyn Fn(&dyn Any) -> Result<(), Error> + Send + Sync>;

/// [`NodeType::inactive`] as a function of the node, its type and the param.
pub type Inactive = fn(&NodeDef, &NodeDesc, &str) -> Option<&'static str>;

/// The runtime state of one node, owned by the cooker, living as long as the node does (so hot
/// reload keeps caches, streams and tracking ids of nodes that did not change).
pub trait Operator: Send {
    /// Produce the outputs. Runs on the cook thread and must not block: anything slow goes into a
    /// job ([`CookContext::spawn`]).
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError>;

    /// State of the node's asynchronous job, if it has one.
    fn job_status(&self) -> Option<JobStatus> {
        None
    }

    /// Whether this instance depends on the show time, if that differs from the type's
    /// declaration ([`NodeDesc::time_dependent`]): a shader that reads the time, a noise with a
    /// speed. Asked before every cook, so it may change with the parameters of the last cook.
    fn time_dependent(&self) -> Option<bool> {
        None
    }

    /// Parameters this instance declares beyond its type's (see [`NodeDesc::extra_params`]), as
    /// known after the last cook.
    fn params(&self) -> Option<Vec<ParamSpec>> {
        None
    }

    /// How input `input`'s pixels relate to this node's output pixels: the same frame for most
    /// nodes. Metadata for `res.analyze`; no behaviour depends on it.
    fn density(&self, def: &NodeDef, desc: &NodeDesc, input: usize) -> Density {
        let _ = (def, desc, input);
        Density::SAME
    }

    /// Whether the `width` and `height` params set this instance's resolution, so
    /// `res.optimize` may change them. False for nodes whose size follows an input or a file.
    fn sets_size(&self, def: &NodeDef, desc: &NodeDesc) -> bool {
        let _ = (def, desc);
        false
    }

    /// State worth keeping across a restart (a feedback loop's last frame, a held cache, later
    /// a tracker's ids), or `None` when there is nothing to keep now. Called on the cook thread
    /// whenever a state snapshot is taken, so it must be cheap: hand out handles (`Data` is an
    /// `Arc`), not copies; the host reads textures back without waiting.
    fn save_state(&self) -> Option<Vec<StateItem>> {
        None
    }

    /// Takes back what [`Operator::save_state`] returned in an earlier run (or earlier in this one,
    /// `state.restore`): the items come from a file on disk and may not fit (another version, another
    /// size), which is an error, never a panic. Called before the operator's first cook when the
    /// state arrives early, otherwise between two cooks; the node cooks again afterwards.
    fn restore_state(&mut self, items: Vec<StateItem>) -> Result<(), String> {
        let _ = items;
        Err("this node keeps no state".into())
    }
}

/// How the pixels of a node's input relate to the pixels of its output, for `res.analyze`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Density {
    /// How far the input's texture coordinates move per unit of the output's, the same
    /// everywhere: rows are the input's u and v, columns the output's u and v. The identity
    /// for a filter and for a resize (the whole frame becomes the whole frame); 0.5 on the
    /// diagonal for a transform that scales up by 2, whose input therefore needs twice the
    /// resolution of its output.
    Linear([[f64; 2]; 2]),
    /// Varies across the picture (a mapper's homography and mesh); the analysis asks the
    /// node's own model.
    Varying,
    /// The input is not shown pixel for pixel (a lookup table): no demand on it.
    Unrelated,
}

impl Density {
    pub const SAME: Density = Density::Linear([[1.0, 0.0], [0.0, 1.0]]);

    /// The input needs `factor` times the output's resolution along each axis (2: the node
    /// magnifies by 2).
    pub fn needs(factor: [f64; 2]) -> Density {
        let inv = |f: f64| if f.is_finite() && f > 1e-9 { 1.0 / f } else { 1.0 };
        Density::Linear([[inv(factor[0]), 0.0], [0.0, inv(factor[1])]])
    }

    /// The resolution (across, down) the input needs when the output needs `out`; `None` for
    /// [`Density::Varying`] and [`Density::Unrelated`]. Along each of the input's axes it is one
    /// over the distance the input moves per needed output pixel, along the output's steepest
    /// direction: exact for scales, rotations and transposes.
    pub fn demand(&self, out: [f64; 2]) -> Option<[f64; 2]> {
        let Density::Linear(j) = self else { return None };
        let axis = |row: [f64; 2]| {
            let len = (row[0] / out[0].max(1e-9)).hypot(row[1] / out[1].max(1e-9));
            if len.is_finite() && len > 1e-12 { 1.0 / len } else { 0.0 }
        };
        let [u, v] = *j;
        Some([axis(u), axis(v)])
    }
}

/// All node types known to a process, by kind.
#[derive(Default, Clone)]
pub struct NodeTypes {
    types: BTreeMap<String, Arc<dyn NodeType>>,
}

impl NodeTypes {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a type after validating its declaration. A kind can be registered only once.
    pub fn register(&mut self, node_type: impl NodeType + 'static) -> Result<(), Error> {
        let desc = node_type.desc();
        desc.validate()?;
        if self.types.contains_key(&desc.kind) {
            return Err(Error::new(format!("node type `{}` registered twice", desc.kind)));
        }
        self.types.insert(desc.kind.clone(), Arc::new(node_type));
        Ok(())
    }

    pub fn get(&self, kind: &str) -> Option<&Arc<dyn NodeType>> {
        self.types.get(kind)
    }

    pub fn desc(&self, kind: &str) -> Option<&NodeDesc> {
        self.types.get(kind).map(|t| t.desc())
    }

    pub fn kinds(&self) -> impl Iterator<Item = &str> + '_ {
        self.types.keys().map(String::as_str)
    }
}

/// A [`NodeType`] made from a declaration and a closure, for simple nodes.
pub struct FnNodeType<F> {
    desc: NodeDesc,
    create: F,
    chooser: Option<InputChooser>,
    warm_up: Option<WarmUp>,
    inactive: Option<Inactive>,
}

impl<F: Fn() -> Box<dyn Operator> + Send + Sync> FnNodeType<F> {
    pub fn new(desc: NodeDesc, create: F) -> Self {
        Self { desc, create, chooser: None, warm_up: None, inactive: None }
    }

    /// Prepares with `warm_up` before the first cook ([`NodeType::warm_up`]).
    pub fn warm_up(
        mut self,
        warm_up: impl Fn(&dyn Any) -> Result<(), Error> + Send + Sync + 'static,
    ) -> Self {
        self.warm_up = Some(Arc::new(warm_up));
        self
    }

    /// Says with `inactive` which params do not count ([`NodeType::inactive`]).
    pub fn inactive(mut self, inactive: Inactive) -> Self {
        self.inactive = Some(inactive);
        self
    }

    /// Needs only the inputs `chooser` picks ([`NodeType::input_chooser`]).
    pub fn choose_inputs(mut self, chooser: InputChooser) -> Self {
        self.chooser = Some(chooser);
        self
    }
}

impl<F: Fn() -> Box<dyn Operator> + Send + Sync> NodeType for FnNodeType<F> {
    fn desc(&self) -> &NodeDesc {
        &self.desc
    }

    fn create(&self) -> Box<dyn Operator> {
        (self.create)()
    }

    fn input_chooser(&self) -> Option<InputChooser> {
        self.chooser
    }

    fn warm_up(&self, resource: &dyn Any) -> Result<(), Error> {
        self.warm_up.as_ref().map_or(Ok(()), |f| f(resource))
    }

    fn inactive(&self, node: &NodeDef, name: &str) -> Option<&'static str> {
        self.inactive.and_then(|f| f(node, &self.desc, name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn density_demand_follows_scale_rotation_and_transpose() {
        let close = |a: Option<[f64; 2]>, b: [f64; 2]| {
            let a = a.unwrap();
            assert!((a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9, "{a:?} vs {b:?}");
        };
        close(Density::SAME.demand([1920.0, 1080.0]), [1920.0, 1080.0]);
        // Magnified by 2 across: the input needs twice the pixels across.
        close(Density::needs([2.0, 1.0]).demand([1920.0, 1080.0]), [3840.0, 1080.0]);
        // Transposed: the input's u runs down the output.
        close(Density::Linear([[0.0, 1.0], [1.0, 0.0]]).demand([1920.0, 1080.0]), [1080.0, 1920.0]);
        // A square output turned by 45 degrees: each input axis runs diagonally.
        let (c, s) = (std::f64::consts::FRAC_1_SQRT_2, std::f64::consts::FRAC_1_SQRT_2);
        close(Density::Linear([[c, -s], [s, c]]).demand([1000.0, 1000.0]), [1000.0, 1000.0]);
        assert_eq!(Density::Varying.demand([1.0, 1.0]), None);
        assert_eq!(Density::Unrelated.demand([1.0, 1.0]), None);
    }

    struct Nothing;
    impl Operator for Nothing {
        fn cook(&mut self, _cx: &CookContext<'_>) -> Result<Cooked, CookError> {
            Ok(Cooked::Unchanged)
        }
    }

    fn ty(desc: NodeDesc) -> FnNodeType<impl Fn() -> Box<dyn Operator> + Send + Sync> {
        FnNodeType::new(desc, || Box::new(Nothing) as Box<dyn Operator>)
    }

    #[test]
    fn registration_validates() {
        let mut types = NodeTypes::new();
        assert!(
            types.register(ty(NodeDesc::new("chan.lfo", "LFO").param(ParamSpec::float("rate", 1.0)))).is_ok()
        );
        assert!(types.register(ty(NodeDesc::new("chan.lfo", "again"))).is_err());
        assert!(types.register(ty(NodeDesc::new("nofamily", ""))).is_err());
        assert!(types.register(ty(NodeDesc::new("Tex.noise", ""))).is_err());
        let twice =
            NodeDesc::new("chan.x", "").param(ParamSpec::float("a", 0.0)).param(ParamSpec::int("a", 0));
        assert!(types.register(ty(twice)).is_err());
        let inputs = NodeDesc::new("chan.y", "").input("a", DataKind::Chan).input("a", DataKind::Chan);
        assert!(types.register(ty(inputs)).is_err());
        let bad_param = NodeDesc::new("chan.z", "").param(ParamSpec::float("x", 9.0).clamp(0.0, 1.0));
        assert!(types.register(ty(bad_param)).is_err());
        assert_eq!(types.kinds().collect::<Vec<_>>(), ["chan.lfo"]);
    }

    #[test]
    fn data_downcasts() {
        let d = Data::new(DataKind::Chan, 2.5f64);
        assert_eq!(d.get::<f64>(), Some(&2.5));
        assert_eq!(d.get::<f32>(), None);
        assert!(d.same(&d.clone()));
        assert!(!d.same(&Data::new(DataKind::Chan, 2.5f64)));
    }
}
