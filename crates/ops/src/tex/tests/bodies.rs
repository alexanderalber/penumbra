//! The geometry `physics.rigid_bodies` hands out: every body placed, each group drawn as its
//! own model with its own material, a group without a model as boxes of 1 m.

use penumbra_core::Value;
use penumbra_gpu::Geo;
use penumbra_graph::Port;

use super::{H, Rig, W, connect, create, gpu, set, size};

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

#[test]
fn an_obstacle_moved_by_a_transform_carries_the_body_on_it() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        // A slab 4 x 4 x 0.2 with its top at z = 1.1, and a box of 1 m resting on it.
        create("/slab", "geo.primitive", &[("size", Value::Vec3([4.0, 4.0, 0.2]))]),
        create("/slab_t", "geo.transform", &[("translate", Value::Vec3([0.0, 0.0, 1.0]))]),
        connect("/slab", "/slab_t", 0),
        create("/t0", "table.text", &[("text", "tz\n1.62".into()), ("format", "csv".into())]),
        create("/b0", "physics.bodies", &[("shape", "box".into())]),
        connect("/t0", "/b0", 0),
        create("/r", "physics.rigid_bodies", &[]),
        connect("/b0", "/r", 0),
        connect("/slab_t", "/r", penumbra_graph::MAX_RUN as u32),
        create("/show", "geo.null", &[]),
        connect("/r", "/show", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    let place = |rig: &mut Rig, frames: usize| -> [f64; 4] {
        for _ in 0..frames {
            rig.clock.advance(1);
            rig.cook("/show");
        }
        let s = rig.cooker.status(rig.id("/r")).unwrap();
        assert_eq!((s.error, s.warning), (None, None));
        let out = rig.cooker.output(Port { node: rig.id("/show"), output: 0 }).unwrap();
        let world = out.get::<Geo>().unwrap().items[0].world;
        // Where the box is, and the sine of its turn about z.
        [world[0][3], world[1][3], world[2][3], world[1][0]]
    };
    let slab_at = |x: f64, z: f64| set("/slab_t.translate", Value::Vec3([x, 0.0, z]));
    let rest = place(&mut rig, 120);
    assert!((rest[2] - 1.6).abs() < 0.02 && rest[0].abs() < 1e-3, "it rests on the slab: {rest:?}");
    // The slab goes 1.5 m along x in 90 frames, then 1 m up in 60: the box goes along.
    for k in 1..=90 {
        rig.edit(&[slab_at(f64::from(k) / 60.0, 1.0)]);
        place(&mut rig, 1);
    }
    let carried = place(&mut rig, 30);
    assert!((carried[0] - 1.5).abs() < 0.15, "carried along: {carried:?}");
    for k in 1..=60 {
        rig.edit(&[slab_at(1.5, 1.0 + f64::from(k) / 60.0)]);
        place(&mut rig, 1);
    }
    let lifted = place(&mut rig, 30);
    assert!((lifted[2] - 2.6).abs() < 0.03, "lifted: {lifted:?}");
    // Turned about its middle by half a degree a frame, the slab turns the box on its middle.
    for k in 1..=60 {
        rig.edit(&[set("/slab_t.rotate", Value::Vec3([0.0, 0.0, f64::from(k) * 0.5]))]);
        place(&mut rig, 1);
    }
    let turned = place(&mut rig, 30);
    assert!((turned[2] - 2.6).abs() < 0.03, "{turned:?}");
    assert!((turned[3] - 0.5).abs() < 0.05, "it went round by the slab's 30 degrees: {turned:?}");
    // A new size builds the slab anew where it is: the box stays on it.
    rig.edit(&[set("/slab_t.scale", Value::Vec3([1.5, 1.5, 1.0]))]);
    let scaled = place(&mut rig, 30);
    assert!((scaled[2] - 2.6).abs() < 0.03, "{scaled:?}");
    // Moved farther than `jump` in a frame, the slab is gone from under the box, which falls
    // to the floor where it was.
    rig.edit(&[slab_at(30.0, 2.0)]);
    let fell = place(&mut rig, 120);
    assert!((fell[2] - 0.5).abs() < 0.03 && (fell[0] - scaled[0]).abs() < 0.05, "{fell:?}");
}

#[test]
fn the_pile_of_the_docs_renders_once_a_camera_is_wired() {
    let Some(gpu) = gpu() else { return };
    let rows = "tx,ty,tz\n0,0,2\n0.2,0,3.5\n-0.1,0.1,5";
    let ops = [
        create("/t", "table.text", &[("text", rows.into()), ("format", "csv".into())]),
        create("/box", "geo.primitive", &[]),
        create("/b", "physics.bodies", &[]),
        connect("/t", "/b", 0),
        connect("/box", "/b", 1),
        create("/r", "physics.rigid_bodies", &[]),
        connect("/b", "/r", 0),
        create("/render", "tex.render", &size()),
        connect("/r", "/render", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    // Table, model, group, solver and render: without a camera the render has no picture.
    rig.cook("/render");
    let error = rig.cooker.status(rig.id("/render")).unwrap().error.unwrap();
    assert!(error.contains("camera"), "{error}");
    // The sixth node: a camera into `camera`.
    rig.edit(&[create("/cam", "view.camera", &[]), connect("/cam", "/render", 3)]);
    for _ in 0..180 {
        rig.clock.advance(1);
        rig.cook("/render");
    }
    for path in ["/render", "/r", "/b"] {
        let s = rig.cooker.status(rig.id(path)).unwrap();
        assert_eq!((s.error, s.warning), (None, None), "{path}");
    }
    // The three boxes lie in a pile at the origin, where the camera looks: the middle of the
    // picture shows them, the corner does not.
    let tex = rig.tex("/render").unwrap();
    let px = rig.gpu.read(&tex).unwrap();
    let (corner, middle) = (px.get(0, 0).unwrap(), px.get(W as u32 / 2, H as u32 / 2).unwrap());
    assert_ne!(corner, middle, "the bodies do not show");
}

#[test]
fn an_obstacle_flattened_by_its_transform_collides_where_it_is_drawn() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        // A slab squeezed to a sheet by a scale of 0 along z, 3 m aside and 2 m up.
        create("/slab", "geo.primitive", &[("size", Value::Vec3([4.0, 4.0, 0.2]))]),
        create(
            "/slab_t",
            "geo.transform",
            &[("translate", Value::Vec3([3.0, 0.0, 2.0])), ("scale", Value::Vec3([1.0, 1.0, 0.0]))],
        ),
        connect("/slab", "/slab_t", 0),
        create("/t0", "table.text", &[("text", "tx,tz\n3,4".into()), ("format", "csv".into())]),
        create("/b0", "physics.bodies", &[("shape", "box".into())]),
        connect("/t0", "/b0", 0),
        create("/r", "physics.rigid_bodies", &[]),
        connect("/b0", "/r", 0),
        connect("/slab_t", "/r", penumbra_graph::MAX_RUN as u32),
        create("/show", "geo.null", &[]),
        connect("/r", "/show", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    for _ in 0..180 {
        rig.clock.advance(1);
        rig.cook("/show");
    }
    let s = rig.cooker.status(rig.id("/r")).unwrap();
    assert_eq!((s.error, s.warning), (None, None));
    let out = rig.cooker.output(Port { node: rig.id("/show"), output: 0 }).unwrap();
    let world = out.get::<Geo>().unwrap().items[0].world;
    let at = [world[0][3], world[1][3], world[2][3]];
    assert!((at[2] - 2.5).abs() < 0.03 && (at[0] - 3.0).abs() < 0.05, "it rests on the sheet: {at:?}");
}
