//! `pts` nodes: points on the GPU ([`penumbra_gpu::Points`]), a list of attributes with an
//! element per point. A generator makes them (`pts.generate`); `geo.from_pts` turns them into
//! geometry that `tex.render` draws. `pts.from_tex` and `tex.from_pts` turn pictures into points
//! and back; `pts.lookup_tex` colours points from a picture. `pts.delete` drops points, after
//! which their count is known on the GPU only ([`penumbra_gpu::points::Count`]).

use penumbra_core::Error;
use penumbra_graph::{DataKind, FnNodeType, NodeDesc, NodeTypes, Operator};

pub mod delete;
pub mod from_tex;
pub mod generate;
pub mod lookup_tex;
pub mod math;
pub mod noise;
pub(crate) mod program;
pub mod random;
pub mod shader;
pub mod to_chan;
pub(crate) mod to_geo;
pub mod to_tex;
pub mod transform;

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    generate::register(types)?;
    to_geo::register(types)?;
    shader::register(types)?;
    noise::register(types)?;
    transform::register(types)?;
    math::register(types)?;
    random::register(types)?;
    from_tex::register(types)?;
    to_tex::register(types)?;
    lookup_tex::register(types)?;
    delete::register(types)?;
    to_chan::register(types)?;
    types.register(FnNodeType::new(
        NodeDesc::new("pts.null", "Passes its input on unchanged: a fixed point to look at, or to wire on from while the chain before it changes")
            .input("in", DataKind::Pts)
            .output("out", DataKind::Pts),
        || Box::new(crate::Null) as Box<dyn Operator>,
    ))?;
    Ok(())
}
