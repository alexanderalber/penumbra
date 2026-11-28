//! `pts` nodes: points on the GPU ([`penumbra_gpu::Points`]), a list of attributes with an
//! element per point. A generator makes them (`pts.generate`); `geo.from_pts` turns them into
//! geometry that `tex.render` draws. `pts.from_tex` and `tex.from_pts` turn pictures into points
//! and back; `pts.lookup_tex` colours points from a picture. `pts.delete` drops points, after
//! which their count is known on the GPU only ([`penumbra_gpu::points::Count`]).
//! `pts.particles` makes points that are born, move and die, pushed by `pts.force` in its loop
//! and masked by `pts.field`; `pts.feedback` closes a loop of any points. `pts.trail` keeps a
//! line behind every point, which `geo.from_pts` draws as ribbons. `pts.proximity` joins near
//! points with lines; `pts.neighbor` counts and averages each point's neighbours.
//! `pts.resample` spaces the points of each line evenly, `pts.smooth` smooths each line.

use penumbra_core::Error;
use penumbra_graph::{DataKind, FnNodeType, NodeDesc, NodeTypes, Operator};

pub mod delete;
pub mod feedback;
pub mod field;
pub mod file;
pub mod force;
pub mod from_geo;
pub mod from_tex;
pub mod from_values;
pub mod generate;
pub mod lookup_tex;
pub mod math;
pub mod merge;
pub mod neighbor;
pub mod noise;
pub mod particles;
pub(crate) mod program;
pub mod proximity;
pub mod random;
pub mod resample;
pub mod shader;
pub mod smooth;
pub mod to_chan;
pub(crate) mod to_geo;
pub mod to_table;
pub mod to_tex;
pub mod trail;
pub mod transform;
pub(crate) mod upload;

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
    particles::register(types)?;
    feedback::register(types)?;
    force::register(types)?;
    field::register(types)?;
    trail::register(types)?;
    proximity::register(types)?;
    resample::register(types)?;
    smooth::register(types)?;
    neighbor::register(types)?;
    merge::register(types)?;
    from_geo::register(types)?;
    from_values::register(types)?;
    to_table::register(types)?;
    file::register(types)?;
    types.register(FnNodeType::new(
        NodeDesc::new("pts.null", "Passes its input on unchanged: a fixed point to look at, or to wire on from while the chain before it changes")
            .input("in", DataKind::Pts)
            .output("out", DataKind::Pts),
        || Box::new(crate::Null) as Box<dyn Operator>,
    ))?;
    Ok(())
}
