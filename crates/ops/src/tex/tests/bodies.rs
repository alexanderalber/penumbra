//! The geometry `physics.rigid_bodies` hands out: every body placed, each group drawn as its
//! own model with its own material, a group without a model as boxes of 1 m.

use penumbra_core::Value;
use penumbra_gpu::Geo;
use penumbra_graph::Port;

use super::{Rig, connect, create, gpu, set};

#[test]
fn the_solver_draws_each_group_as_its_own_model() {
    let Some(gpu) = gpu() else { return };
    let text = |t: &str| [("text", Value::from(t)), ("format", Value::from("csv"))];
    let ops = [
        create("/cube", "geo.primitive", &[("size", Value::Vec3([0.4; 3]))]),
        create("/mat", "mat.constant", &[]),
        create("/red_cube", "geo.assign_material", &[]),
        connect("/cube", "/red_cube", 0),
        connect("/mat", "/red_cube", 1),
        create("/t0", "table.text", &text("tx,tz\n0,3\n2,3")),
        create("/b0", "physics.bodies", &[("shape", "box".into())]),
        connect("/t0", "/b0", 0),
        connect("/red_cube", "/b0", 1),
        create("/t1", "table.text", &text("tx,tz,sx\n5,1,2")),
        create("/b1", "physics.bodies", &[]),
        connect("/t1", "/b1", 0),
        // Nothing pulls, nothing is in the way: the bodies stay where their tables put them.
        create("/r", "physics.rigid_bodies", &[("gravity", Value::Vec3([0.0; 3])), ("floor", false.into())]),
        connect("/b0", "/r", 0),
        connect("/b1", "/r", 1),
        create("/show", "geo.null", &[]),
        connect("/r", "/show", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    let geo = |rig: &mut Rig| -> Geo {
        rig.clock.advance(1);
        rig.cook("/show");
        let s = rig.cooker.status(rig.id("/r")).unwrap();
        assert_eq!((s.error, s.warning), (None, None));
        rig.cooker.output(Port { node: rig.id("/show"), output: 0 }).unwrap().get::<Geo>().unwrap().clone()
    };
    let cube = {
        rig.cook("/cube");
        let out = rig.cooker.output(Port { node: rig.id("/cube"), output: 0 }).unwrap();
        out.get::<Geo>().unwrap().items[0].model.id
    };
    let drawn = geo(&mut rig);
    assert_eq!(drawn.items.len(), 3);
    let at = |k: usize| [drawn.items[k].world[0][3], drawn.items[k].world[1][3], drawn.items[k].world[2][3]];
    assert_eq!((at(0), at(1), at(2)), ([0.0, 0.0, 3.0], [2.0, 0.0, 3.0], [5.0, 0.0, 1.0]));
    // Group 0 is the cube with its material; group 1 has no model and shows a box of 1 m,
    // twice as wide along x as its row says.
    for item in &drawn.items[..2] {
        assert_eq!(item.model.id, cube);
        assert!(item.material.is_some());
    }
    let stand_in = &drawn.items[2];
    assert_ne!(stand_in.model.id, cube);
    assert!(stand_in.material.is_none());
    assert!(stand_in.model.mesh.vertices.iter().all(|v| v.position.iter().all(|c| c.abs() == 0.5)));
    assert_eq!(stand_in.world[0][0], 2.0);
    // Every body has part ids of its own, as copies of geo.instance have.
    let offsets: Vec<u32> = drawn.items.iter().map(|i| i.part_offset).collect();
    assert!(offsets[0] < offsets[1] && offsets[1] < offsets[2], "{offsets:?}");
    // With gravity the bodies move, and the geometry follows them.
    rig.edit(&[set("/r.gravity", Value::Vec3([0.0, 0.0, -9.81]))]);
    for _ in 0..5 {
        geo(&mut rig);
    }
    let fallen = geo(&mut rig);
    assert!(fallen.items[0].world[2][3] < 3.0 && fallen.items[2].world[2][3] < 1.0);
}
