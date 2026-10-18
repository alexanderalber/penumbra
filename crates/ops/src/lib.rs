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
}
