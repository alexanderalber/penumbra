//! `vol` nodes: volumes, grids of cells filling a box in space ([`penumbra_gpu::Medium`]). A
//! simulation makes one (`vol.smoke`, fed by `vol.emitter`), `tex.raymarch` draws it.

use penumbra_core::Error;
use penumbra_graph::{DataKind, FnNodeType, NodeDesc, NodeTypes, Operator};

pub(crate) mod emitter;
pub(crate) mod smoke;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    emitter::register(types)?;
    smoke::register(types)?;
    types.register(FnNodeType::new(
        NodeDesc::new("vol.null", "Passes its input on unchanged: a fixed point to look at, or to wire on from while the chain before it changes")
            .input("in", DataKind::Vol)
            .output("out", DataKind::Vol),
        || Box::new(crate::Null) as Box<dyn Operator>,
    ))?;
    Ok(())
}
