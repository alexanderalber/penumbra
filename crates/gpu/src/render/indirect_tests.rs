//! Copies whose number is kept on the GPU (`Instances::live`), drawn on WARP: never more than
//! `Instances::count`, whatever the number on the GPU says.

use std::sync::Arc;

use super::*;
use crate::geo::{IDENTITY, Instances, Item, Model};
use crate::points::{Attr, AttrType, Points};
use crate::{Buffer, BufferRing, TexRing};

/// Which of four squares at x = -0.6, -0.2, 0.2 and 0.6 show, drawn as copies of which the GPU
/// keeps `live` and the CPU allows `count`.
fn squares(gpu: &Gpu, count: u32, live: u32) -> [bool; 4] {
    let (model, _) = Model::upload(gpu, penumbra_mesh::prim::plane([0.2, 0.2]).unwrap(), "square").unwrap();
    let pos: Vec<u32> =
        [-0.6f32, -0.2, 0.2, 0.6].iter().flat_map(|x| [*x, 0.0, 0.0, 1.0]).map(f32::to_bits).collect();
    let buffer = Buffer::of::<u32>(gpu, pos.len() as u64, "position").unwrap();
    buffer.write(gpu, 0, &pos).unwrap();
    let points = Points::new(4, 4, vec![Attr::new("position", AttrType::Vec3, buffer).unwrap()]).unwrap();
    let position = points
        .attr_texture(gpu, "position", &mut TexRing::default(), &mut BufferRing::default())
        .unwrap()
        .unwrap();
    let header = Buffer::of::<u32>(gpu, 4, "count").unwrap();
    header.write(gpu, 0, &[live, 0, 0, 0]).unwrap();
    let instances = Instances { count: Some(count), live: Some(header), ..Instances::of(position) };
    let item = Item {
        model: Arc::new(model),
        world: IDENTITY,
        part_offset: 0,
        material: None,
        instances: Some(Arc::new(instances)),
    };
    let geo = Geo { items: vec![item], preview: None };
    // Seen along +y: x stays x, z is up, y is the depth.
    let clip = [[1.0, 0.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 1.0, 0.0, 0.5], [0.0, 0.0, 0.0, 1.0]];
    let view =
        View { clip_from_world: clip, view_z: [0.0, 1.0, 0.0, 0.5], eye: [0.0, -10.0, 0.0], parallel: true };
    let look = Look {
        surface: Surface::Material,
        shade_all: false,
        background: [0.0; 4],
        cull: false,
        antialias: false,
        overlay: None,
        shadow_size: 0,
        ao: None,
    };
    let target = Tex::new(gpu, 80, 16, "squares").unwrap();
    Renderer::new().render(gpu, &geo, &[], &view, &look, None, &target, None).unwrap();
    let px = gpu.read(&target).unwrap();
    // The middle of each square: x = -0.6 is pixel 16, then every 16 pixels.
    [16u32, 32, 48, 64].map(|x| px.get(x, 8).is_some_and(|p| p[3] > 0.5))
}

#[test]
fn copies_on_the_gpu_never_pass_the_count() {
    let Some(gpu) = crate::testing::gpu() else { return };
    assert_eq!(squares(&gpu, 4, 3), [true, true, true, false], "three alive of four");
    assert_eq!(squares(&gpu, 2, 4), [true, true, false, false], "the count holds the number on the GPU");
    assert_eq!(squares(&gpu, 2, 1000), [true, true, false, false], "a number far past the texels");
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}
