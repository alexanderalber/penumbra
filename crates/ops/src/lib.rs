//! Built-in node types: `chan` and `table` nodes on the CPU, `tex` nodes on the GPU, the projector
//! DMX and movie outputs and the Python script node.

pub mod audio;
pub mod chan;
pub mod comp;
pub mod dmx;
pub mod freeview;
pub mod geo;
pub mod light;
pub mod mapping;
pub mod mat;
pub mod meters;
pub mod osc;
pub mod out;
pub mod res;
pub mod script;
pub mod shapes;
pub mod table;
pub mod tex;
pub mod trace;
pub mod view;

use std::any::Any;

use penumbra_core::Error;
use penumbra_gpu::Gpu;
use penumbra_graph::{CookContext, CookError, Cooked, NodeTypes, Operator};

/// Registers every built-in node type.
pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    audio::register(types)?;
    chan::register(types)?;
    comp::register(types)?;
    dmx::register(types)?;
    geo::register(types)?;
    light::register(types)?;
    mat::register(types)?;
    osc::register(types)?;
    out::register(types)?;
    script::register(types)?;
    table::register(types)?;
    tex::register(types)?;
    view::register(types)?;
    Ok(())
}

/// The error of a node whose input has nothing (yet): still pending while one is wired,
/// otherwise a hint to wire one.
pub(crate) fn waiting(cx: &CookContext<'_>) -> CookError {
    waiting_for(cx, "connect an input")
}

/// [`waiting`] with `hint` for the unwired case.
pub(crate) fn waiting_for(cx: &CookContext<'_>, hint: &'static str) -> CookError {
    if cx.def().inputs.iter().any(Option::is_some) { CookError::Pending } else { CookError::failed(hint) }
}

/// The GPU, for nodes that need one; an error until it arrives.
pub(crate) fn gpu<'a>(cx: &'a CookContext<'_>) -> Result<&'a Gpu, CookError> {
    cx.resource::<Gpu>().ok_or_else(|| CookError::failed("no GPU yet"))
}

/// A warm-up hook ([`penumbra_graph::WarmUp`]) that runs `f` with the GPU.
pub(crate) fn on_gpu(
    f: impl Fn(&Gpu) -> Result<(), Error> + Send + Sync + 'static,
) -> impl Fn(&dyn Any) -> Result<(), Error> + Send + Sync + 'static {
    move |resource| resource.downcast_ref::<Gpu>().map_or(Ok(()), &f)
}

/// Compiles the shaders and pipelines of every node type in `types` ([`penumbra_graph::NodeType::warm_up`]), so
/// the first node of each kind does not stall the cook thread (a pipeline takes up to about a
/// hundred milliseconds on some backends). Call it on a background thread when the GPU appears.
pub fn warm_up(types: &NodeTypes, gpu: &Gpu) {
    for kind in types.kinds() {
        if let Some(t) = types.get(kind)
            && let Err(e) = t.warm_up(gpu)
        {
            log::error!("the shaders of {kind} do not compile: {e}");
        }
    }
}

/// Passes its input on as it is (`chan.null`, `table.null`).
pub(crate) struct Null;

impl Operator for Null {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        match cx.input(0) {
            Some(data) => Ok(Cooked::Outputs(vec![data.clone()])),
            None => Err(waiting(cx)),
        }
    }
}

/// All built-in node types.
pub fn builtin_types() -> Result<NodeTypes, Error> {
    let mut types = NodeTypes::new();
    register(&mut types)?;
    Ok(types)
}

#[cfg(test)]
mod port_docs {
    /// Wiring the wrong one of several outputs is easy (the editor names them, but `tex.render`'s
    /// `data` looks like a picture of its own), so each says what it holds.
    #[test]
    fn every_output_of_a_node_with_several_is_described() {
        let types = super::builtin_types().unwrap();
        for kind in types.kinds() {
            let desc = types.desc(kind).unwrap();
            if desc.outputs.len() > 1 {
                for o in &desc.outputs {
                    assert!(!o.doc.is_empty(), "{kind} output `{}` has no description", o.name);
                }
            }
        }
    }

    /// The param panel's help overlay and the reference show these texts; a param nobody
    /// explained leaves its user guessing. A text of three words or fewer ("Wave form", "In
    /// tiles") says too little to count, and each value of a choice says what it does.
    #[test]
    fn every_param_is_described() {
        let types = super::builtin_types().unwrap();
        let mut missing = Vec::new();
        let mut total = 0;
        for kind in types.kinds() {
            for p in &types.desc(kind).unwrap().params {
                total += 1;
                if p.full_doc().split_whitespace().count() <= 3 {
                    missing.push(format!("{kind}.{} ({:?})", p.name, p.full_doc()));
                }
                if let penumbra_core::ParamKind::Choice(values) = &p.kind {
                    let bare: Vec<&str> =
                        values.iter().filter(|v| p.doc_of_choice(v).is_none()).map(String::as_str).collect();
                    if !bare.is_empty() {
                        missing.push(format!("{kind}.{}: no text for {}", p.name, bare.join(", ")));
                    }
                }
            }
        }
        assert!(
            missing.is_empty(),
            "{} problems in the descriptions of {total} params (four words or more, every choice):
{}",
            missing.len(),
            missing.join(
                "
"
            )
        );
    }
}
