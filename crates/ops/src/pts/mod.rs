//! `pts` nodes: points on the GPU ([`penumbra_gpu::Points`]), a list of attributes with an
//! element per point. A generator makes them (`pts.generate`); `geo.from_pts` turns them into
//! geometry that `tex.render` draws.

use penumbra_core::Error;
use penumbra_graph::{DataKind, FnNodeType, NodeDesc, NodeTypes, Operator};

pub mod generate;
pub(crate) mod to_geo;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    generate::register(types)?;
    to_geo::register(types)?;
    types.register(FnNodeType::new(
        NodeDesc::new("pts.null", "Passes its input on unchanged: a fixed point to look at, or to wire on from while the chain before it changes")
            .input("in", DataKind::Pts)
            .output("out", DataKind::Pts),
        || Box::new(crate::Null) as Box<dyn Operator>,
    ))?;
    Ok(())
}
