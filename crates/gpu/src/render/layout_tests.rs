//! The hand-packed uniforms of render.rs against the structs and constants of render.wgsl and
//! ao.wgsl, read with naga: sizes, array lengths, and every member's offset (each member gets a
//! value of its own, read back where naga puts it). The packing pads to fixed sizes, so without
//! these a member added on one side only would go unnoticed.

use super::*;
use crate::geo::{IDENTITY, Wire};
use crate::testing::{f32_at, member, u32_at, wgsl_f32, wgsl_globals, wgsl_struct};

const RENDER: &str = SOURCE;
const AO: &str = include_str!("../ao.wgsl");

/// A matrix whose first column-major element is `v` and the rest the identity.
fn mat(v: f64) -> Mat4 {
    let mut m = IDENTITY;
    m[0][0] = v;
    m
}

fn light() -> (Light, Placed) {
    let light = Light {
        cone: [0.125, 0.375],
        inverse_square: true,
        bias: 2.0,
        ..Light::point([1.5, 0.0, 0.0], [0.25, 0.5, 0.75])
    };
    let placed = Placed { clip: mat(7.0), layer: Some(4), slot: Some(5), dropped: false };
    (light, placed)
}

fn view() -> View {
    View { clip_from_world: mat(3.0), view_z: [0.5, 0.0, 0.0, 0.0], eye: [9.0, 0.0, 0.0], parallel: false }
}

#[test]
fn constants_match_the_shader() {
    assert_eq!(wgsl_f32(RENDER, "OPAQUE"), OPAQUE);
    let pictures = wgsl_globals(RENDER)
        .iter()
        .filter(|n| n.strip_prefix("picture").is_some_and(|k| k.parse::<u32>().is_ok()))
        .count();
    assert_eq!(pictures, MAX_PROJECTORS);
}

#[test]
fn light_layout_matches_the_shader() {
    let (span, m) = wgsl_struct(RENDER, "Light");
    assert_eq!(u64::from(span), LIGHT_BYTES);
    let (l, p) = light();
    let b = light_bytes(&l, &p);
    assert_eq!(b.len() as u64, LIGHT_BYTES);
    let at = |name: &str, plus: u32| f32_at(&b, member(&m, name).offset + plus);
    assert_eq!(at("position", 0), 1.5);
    assert_eq!(at("position", 12), 2.0, "the kind: point");
    assert_eq!(at("direction", 12), 0.125);
    assert_eq!(at("color", 0), 0.25);
    assert_eq!(at("color", 12), 0.375);
    assert_eq!(at("extra", 0), 1.0);
    assert_eq!(at("extra", 4), 5.0);
    assert_eq!(at("extra", 8), 6.0);
    assert_eq!(at("extra", 12), 2.0);
    assert_eq!(at("clip", 0), 7.0);
}

#[test]
fn frame_layout_matches_the_shader() {
    let (span, m) = wgsl_struct(RENDER, "Frame");
    assert_eq!(u64::from(span), FRAME_BYTES);
    let lights = member(&m, "lights");
    assert_eq!(lights.len, Some(MAX_LIGHTS as u32));
    assert_eq!(u64::from(lights.offset), FRAME_BYTES - MAX_LIGHTS as u64 * LIGHT_BYTES);
    let look = Look {
        surface: Surface::Texture,
        shade_all: true,
        background: [0.0; 4],
        cull: false,
        antialias: false,
        overlay: Some(Overlay { factor: 11.0, scale: [12.0, 13.0], cap: 14.0 }),
        shadow_size: 1024,
        ao: None,
    };
    let maps = Maps { shadow: 1024, tint: 512, tinted: true, ao: true };
    let (l, p) = light();
    let b = frame_bytes(&view(), &look, maps, &[l], &[p]);
    assert_eq!(b.len() as u64, FRAME_BYTES);
    let f = |name: &str| f32_at(&b, member(&m, name).offset);
    let u = |name: &str| u32_at(&b, member(&m, name).offset);
    assert_eq!(f("clip"), 3.0);
    assert_eq!(f("view_z"), 0.5);
    assert_eq!(f("eye"), 9.0);
    assert_eq!(u("mode"), 1);
    assert_eq!(u("light_count"), 1);
    assert_eq!(u("shade_all"), 1);
    assert_eq!(u("overlay"), 1);
    assert_eq!(f("overlay_scale"), 12.0);
    assert_eq!(f("overlay_factor"), 11.0);
    assert_eq!(f("overlay_cap"), 14.0);
    assert_eq!(f("shadow"), 1024.0);
    assert_eq!(f("lights"), 1.5);
}

#[test]
fn draw_layout_matches_the_shader() {
    let (span, m) = wgsl_struct(RENDER, "Draw");
    assert_eq!(u64::from(span), DRAW_BYTES);
    let material = Material {
        shading: Shading::Phong,
        specular: [0.5, 0.0, 0.0],
        shininess: 8.0,
        emit: [0.25, 0.0, 0.0],
        metallic: 0.125,
        roughness: 0.75,
        wire: Some(Wire { width: 3.0, polygons: true, faces: Faces::Filled, fill: [0.625, 0.0, 0.0, 0.0] }),
        ..Material::constant()
    };
    let normals = [[2.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let b = draw_slot(&mat(4.0), &normals, [0.875, 0.0, 0.0, 1.0], &material, false, 17, None);
    assert_eq!(b.len() as u64, DRAW_BYTES);
    let at = |name: &str, plus: u32| f32_at(&b, member(&m, name).offset + plus);
    assert_eq!(at("world", 0), 4.0);
    assert_eq!(at("n0", 0), 2.0);
    assert_eq!(at("color", 0), 0.875);
    assert_eq!(at("specular", 0), 0.5);
    assert_eq!(at("specular", 12), 8.0);
    assert_eq!(at("emit", 0), 0.25);
    assert_eq!(u32_at(&b, member(&m, "part").offset), 17);
    assert_eq!(u32_at(&b, member(&m, "shading").offset), 1);
    assert_eq!(at("metallic", 0), 0.125);
    assert_eq!(at("roughness", 0), 0.75);
    assert_eq!(at("wire", 0), 3.0);
    assert_eq!(at("wire", 4), 1.0);
    assert_eq!(at("wire", 8), 1.0);
    assert_eq!(at("fill", 0), 0.625);
    assert_eq!(u32_at(&b, member(&m, "inst").offset), 0);
}

#[test]
fn ao_layout_matches_the_shader() {
    let (span, m) = wgsl_struct(AO, "Ao");
    assert_eq!(u64::from(span), AO_BYTES);
    let b = ao_bytes(&view(), (640, 480), Ao { radius: 0.375, strength: 0.625 });
    assert_eq!(b.len() as u64, AO_BYTES);
    let f = |name: &str| f32_at(&b, member(&m, name).offset);
    assert_eq!(f("clip"), 3.0);
    assert_eq!(f("view_z"), 0.5);
    assert_eq!(f("size"), 640.0);
    assert_eq!(f("radius"), 0.375);
    assert_eq!(f("strength"), 0.625);
}
