//! `light` nodes: ambient, directional, point and spot lights, wired to a render's `lights` input
//! (several through `light.merge`). Only phong materials answer them; there is no light by default.
//!
//! `light.projector` is (a camera from a `view` node throwing its texture input onto the geometry,
//! TouchDesigner's projector map); shadows exist for directional, spot and projector lights.
//!
//! `light.environment` is an equirectangular picture of everything around, baked into maps for
//! diffuse light and reflections.
//!
//! The data on a `light` connection is [`Lights`]: a list, one light per light node, merged
//! lists concatenated.

use std::sync::Arc;

use penumbra_core::{Error, ParamSpec, Unit};
use penumbra_gpu::environment::{EnvironmentMaps, WIDTHS};
use penumbra_gpu::render::{Environment, Light, LightKind, Projector};
use penumbra_gpu::{Lights, Tex};
use penumbra_graph::{
    CookContext, CookError, Cooked, Data, DataKind, FnNodeType, NodeDesc, NodeTypes, Operator,
};

use crate::view::{Lens, Viewpoint};

/// Choices of `falloff`.
pub const FALLOFF: [&str; 2] = ["none", "inverse square"];

pub fn register(types: &mut NodeTypes) -> Result<(), Error> {
    types.register(FnNodeType::new(
        NodeDesc::new(
            "light.ambient",
            "Even light from every side, also on surfaces that face away: lifts the shadows",
        )
        .param(color())
        .param(intensity(0.2))
        .output("out", DataKind::Light),
        || Box::new(LightNode(LightKind::Ambient)) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new("light.directional", "Parallel light from far away, like the sun")
            .param(
                ParamSpec::vec3("position", [-2.0, -3.0, 4.0])
                    .range(-10.0, 10.0)
                    .doc("Where the light shines from; only the direction to the target counts"),
            )
            .param(target())
            .param(color())
            .param(intensity(1.0))
            .param(shadows(false))
            .param(bias())
            .output("out", DataKind::Light),
        || Box::new(LightNode(LightKind::Directional)) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new("light.point", "Light from a point in every direction, like a bare bulb")
            .param(position([1.5, -2.5, 2.5]))
            .param(color())
            .param(intensity(1.0))
            .param(falloff())
            .output("out", DataKind::Light),
        || Box::new(LightNode(LightKind::Point)) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new("light.spot", "Light from a point in a cone towards a target, like a stage spot")
            .param(position([0.0, -3.0, 3.0]))
            .param(target())
            .param(
                ParamSpec::float("angle", 45.0)
                    .clamp(1.0, 179.0)
                    .range(5.0, 120.0)
                    .unit(Unit::Degrees)
                    .doc("The whole cone, edge to edge"),
            )
            .param(
                ParamSpec::float("softness", 0.2)
                    .clamp(0.0, 1.0)
                    .doc("How much of the cone fades towards its edge; 0 is a hard edge"),
            )
            .param(color())
            .param(intensity(1.0))
            .param(shadows(false))
            .param(falloff())
            .param(bias())
            .output("out", DataKind::Light),
        || Box::new(LightNode(LightKind::Spot)) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new(
            "light.projector",
            "A projector in the 3D scene: throws its picture onto the geometry from where a camera node stands",
        )
        .input("camera", DataKind::View)
        .input("texture", DataKind::Tex)
        .param(color())
        .param(intensity(1.0))
        .param(shadows(true))
        .param(falloff())
        .param(bias())
        .output("out", DataKind::Light),
        || Box::new(ProjectorNode) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new(
            "light.environment",
            "Light from a panorama picture of the surroundings (equirectangular): soft, natural light and reflections",
        )
        .input("texture", DataKind::Tex)
        .param(color())
        .param(intensity(1.0))
        .param(
            ParamSpec::float("turn", 0.0)
                .range(-180.0, 180.0)
                .unit(Unit::Degrees)
                .doc("About z, counter-clockwise seen from above; at 0 the picture's middle looks along +x, as in Blender"),
        )
        .param(
            ParamSpec::int("size", 1024)
                .clamp(f64::from(WIDTHS.0), f64::from(WIDTHS.1))
                .range(128.0, 2048.0)
                .more()
                .doc("Texels across the baked reflections: sharper mirrors, more memory and a longer bake"),
        )
        .output("out", DataKind::Light),
        || Box::new(EnvironmentNode::default()) as Box<dyn Operator>,
    ))?;
    types.register(FnNodeType::new(
        NodeDesc::new("light.merge", "Combines several lights for one render")
            .input("in0", DataKind::Light)
            .input("in1", DataKind::Light)
            .input("in2", DataKind::Light)
            .input("in3", DataKind::Light)
            .output("out", DataKind::Light),
        || Box::new(Merge) as Box<dyn Operator>,
    ))?;
    Ok(())
}

fn position(default: [f64; 3]) -> ParamSpec {
    ParamSpec::vec3("position", default).range(-10.0, 10.0).doc("Where the light is, in metres (z up)")
}

fn target() -> ParamSpec {
    ParamSpec::vec3("target", [0.0; 3])
        .range(-10.0, 10.0)
        .doc("The point the light shines at, in metres (z up)")
}

fn color() -> ParamSpec {
    ParamSpec::color("color", [1.0; 4])
        .doc("Colour of the light, linear; intensity scales it, alpha is unused")
}

fn intensity(default: f64) -> ParamSpec {
    ParamSpec::float("intensity", default)
        .clamp(0.0, 1e4)
        .range(0.0, 4.0)
        .doc("Brightness: multiplies the colour; 0 is off, 2 twice as bright")
}

fn shadows(default: bool) -> ParamSpec {
    ParamSpec::bool("shadows", default).doc("What stands in the light's way casts a shadow")
}

fn bias() -> ParamSpec {
    ParamSpec::float("bias", 1.0)
        .clamp(0.0, 20.0)
        .range(0.0, 4.0)
        .more()
        .doc("Shadow offset, times the usual. Raise it against stripes on lit surfaces, lower it against light leaking where objects touch")
}

fn falloff() -> ParamSpec {
    ParamSpec::choice("falloff", &FALLOFF)
        .more()
        .doc("How the light weakens with distance")
        .choice_doc("none", "as bright at any distance")
        .choice_doc(
            "inverse square",
            "as bright as colour and intensity say at 1 m, a quarter of that at 2 m",
        )
}

fn vec3(cx: &CookContext<'_>, name: &str) -> Result<[f64; 3], CookError> {
    match cx.param(name).and_then(|v| v.components()) {
        Some(&[x, y, z]) => Ok([x, y, z]),
        _ => Err(CookError::failed(format!("`{name}` must be three numbers"))),
    }
}

/// `color` times `intensity`.
fn light_color(cx: &CookContext<'_>) -> Result<[f32; 3], CookError> {
    let k = cx.float("intensity")?;
    match cx.param("color").and_then(|v| v.components()) {
        Some([r, g, b, ..]) => Ok([r, g, b].map(|c| (c * k) as f32)),
        _ => Err(CookError::failed("`color` must be a colour")),
    }
}

/// `shadows` and `bias`, where the node has them.
fn shadow_params(cx: &CookContext<'_>, light: &mut Light) -> Result<(), CookError> {
    if cx.param("shadows").is_some() {
        light.shadows = cx.bool("shadows")?;
        light.bias = cx.float("bias")? as f32;
    }
    Ok(())
}

struct LightNode(LightKind);

impl Operator for LightNode {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let mut light = Light::ambient(light_color(cx)?);
        light.kind = self.0;
        shadow_params(cx, &mut light)?;
        if matches!(self.0, LightKind::Point | LightKind::Spot) {
            light.position = vec3(cx, "position")?;
            light.inverse_square = cx.text("falloff")? == "inverse square";
        }
        if matches!(self.0, LightKind::Directional | LightKind::Spot) {
            let (from, to) = (vec3(cx, "position")?, vec3(cx, "target")?);
            let d = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
            if d.iter().all(|v| v.abs() < 1e-9) {
                return Err(CookError::failed("`position` and `target` are the same point"));
            }
            light.direction = d;
        }
        if self.0 == LightKind::Spot {
            let half = (0.5 * cx.float("angle")?).to_radians();
            let soft = cx.float("softness")?.clamp(0.0, 1.0);
            light.cone = [half.cos() as f32, (half * (1.0 - soft)).cos() as f32];
        }
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Light, Lights(vec![light]))]))
    }
}

/// The picture's size a projector without a texture is framed for: a 16:9 projector.
const UNTEXTURED: (u32, u32) = (1920, 1080);

struct ProjectorNode;

impl Operator for ProjectorNode {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let wired = |k: usize| cx.def().inputs.get(k).is_some_and(Option::is_some);
        let Some(viewpoint) = cx.input_as::<Viewpoint>(0) else {
            return Err(if wired(0) { CookError::Pending } else { CookError::failed("connect a camera") });
        };
        let texture = cx.input_as::<Tex>(1).cloned();
        if texture.is_none() && wired(1) {
            return Err(CookError::Pending);
        }
        // Framed for its picture: a texture's size, else what a calibrated lens was solved for.
        let size = match (&texture, viewpoint.lens) {
            (Some(t), _) => t.size(),
            (None, Lens::Calibrated(c)) => (c.size[0].max(1), c.size[1].max(1)),
            (None, Lens::Fov { .. } | Lens::Ortho { .. }) => UNTEXTURED,
        };
        // An orthographic camera throws parallel, as a directional light inside its box.
        let clip_from_world = viewpoint
            .clip_from_world(size, |m| cx.warn(m))
            .ok_or_else(|| CookError::failed("the camera cannot be projected from"))?;
        let mut light = Light::ambient(light_color(cx)?);
        light.kind = LightKind::Projector;
        light.position = viewpoint.position();
        light.direction = viewpoint.forward();
        light.inverse_square = cx.text("falloff")? == "inverse square";
        shadow_params(cx, &mut light)?;
        light.projector = Some(Projector { clip_from_world, texture });
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Light, Lights(vec![light]))]))
    }
}

/// Bakes its picture again only when the picture or the size changes.
#[derive(Default)]
struct EnvironmentNode {
    baked: Option<(u32, Arc<EnvironmentMaps>)>,
}

impl Operator for EnvironmentNode {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let gpu = crate::gpu(cx)?;
        let Some(picture) = cx.input_as::<Tex>(0) else {
            return Err(if cx.def().inputs.first().is_some_and(Option::is_some) {
                CookError::Pending
            } else {
                CookError::failed("connect a picture of the surroundings (equirectangular, 2:1)")
            });
        };
        let size = u32::try_from(cx.int("size")?).unwrap_or(1024);
        let maps = match &self.baked {
            Some((s, maps)) if *s == size && !cx.inputs_changed() => maps.clone(),
            _ => {
                let maps = Arc::new(EnvironmentMaps::bake(gpu, picture, size).map_err(CookError::from)?);
                self.baked = Some((size, maps.clone()));
                maps
            }
        };
        let mut light = Light::ambient(light_color(cx)?);
        light.kind = LightKind::Environment;
        light.environment = Some(Environment { maps, turn: cx.float("turn")? });
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Light, Lights(vec![light]))]))
    }
}

struct Merge;

impl Operator for Merge {
    fn cook(&mut self, cx: &CookContext<'_>) -> Result<Cooked, CookError> {
        let mut all = Vec::new();
        let mut any = false;
        for k in 0..cx.def().inputs.len() {
            let Some(l) = cx.input_as::<Lights>(k) else { continue };
            any = true;
            all.extend(l.0.iter().cloned());
        }
        if !any {
            return Err(crate::waiting_for(cx, "connect a light"));
        }
        Ok(Cooked::Outputs(vec![Data::new(DataKind::Light, Lights(all))]))
    }
}
