//! Built-in node types: `chan` and `table` nodes on the CPU, `tex` nodes on the GPU, the projector
//! output and the Python script node.

pub mod audio;
pub mod chan;
pub mod comp;
pub mod freeview;
pub mod geo;
pub mod light;
pub mod mapping;
pub mod mat;
pub mod osc;
pub mod out;
pub mod res;
pub mod script;
pub mod shapes;
pub mod table;
pub mod tex;
pub mod view;

use penumbra_core::Error;
use penumbra_graph::NodeTypes;

pub use tex::warm_up;

/// Registers every built-in node type.
pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    audio::register(types)?;
    chan::register(types)?;
    comp::register(types)?;
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
