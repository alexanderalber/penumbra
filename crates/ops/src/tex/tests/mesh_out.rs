//! `out.mesh` on WARP: geometry written to OBJ, STL and glb and read back as `geo.file` reads
//! it; matrices baked in; copies from `geo.tex_instance` expanded or skipped; the budget.

use std::path::Path;
use std::time::{Duration, Instant};

use penumbra_core::Value;
use penumbra_mesh::{Bounds, Limits, Mesh, Up};

use super::{Rig, connect, create, gpu, scratch, set};
use crate::out::mesh::MeshOuts;

/// The file as `geo.file` with `up: auto` reads it: z up.
fn read_back(path: &Path) -> Mesh {
    let mut mesh = penumbra_mesh::load_file(path, &Limits::default()).unwrap().mesh;
    mesh.to_z_up(Up::of_file(&path.to_string_lossy()));
    mesh
}

/// Cooks `/w` until the file holds something other than `before`, and the node is done.
fn written(rig: &mut Rig, outs: &MeshOuts, file: &Path, before: &[u8]) -> Mesh {
    let id = rig.id("/w");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        rig.clock.advance(1);
        rig.gpu.poll();
        rig.cook("/w");
        let bytes = std::fs::read(file).unwrap_or_default();
        let done = outs.status(id).is_some_and(|s| !s.writing);
        if done && !bytes.is_empty() && bytes != before {
            return read_back(file);
        }
        let s = rig.cooker.status(id).unwrap();
        assert!(s.error.is_none(), "{s:?}");
        assert!(Instant::now() < deadline, "{} was not written: {s:?}", file.display());
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn close(a: [f32; 3], b: [f32; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4)
}

fn rig_with_outs(ops: &[penumbra_graph::Op], dir: &Path) -> Option<(Rig, MeshOuts)> {
    let gpu = gpu()?;
    let mut rig = Rig::new(gpu, Some(dir), ops);
    let outs = MeshOuts::new();
    rig.cooker.provide(outs.clone());
    Some((rig, outs))
}

#[test]
fn writes_obj_stl_and_glb_with_the_matrices_baked_in() {
    let dir = scratch("mesh-out");
    let ops = vec![
        create("/box", "geo.primitive", &[("size", Value::Vec3([1.0, 1.0, 1.0]))]),
        create("/ball", "geo.primitive", &[("shape", "sphere".into()), ("segments", Value::Int(12))]),
        // The box moved up by 2 and stretched along x.
        create(
            "/up",
            "geo.transform",
            &[("translate", Value::Vec3([0.0, 0.0, 2.0])), ("scale", Value::Vec3([3.0, 1.0, 1.0]))],
        ),
        create("/both", "geo.merge", &[]),
        create("/w", "out.mesh", &[]),
        connect("/box", "/up", 0),
        connect("/up", "/both", 0),
        connect("/ball", "/both", 1),
        connect("/both", "/w", 0),
    ];
    let Some((mut rig, outs)) = rig_with_outs(&ops, dir.path()) else { return };
    let ball = penumbra_mesh::prim::sphere(0.5, 12).unwrap();
    let triangles = 12 + ball.triangle_count();
    // Empty file: export/<node name>.obj.
    let obj = written(&mut rig, &outs, &dir.join("export").join("w.obj"), &[]);
    assert_eq!(obj.triangle_count(), triangles);
    assert_eq!(obj.parts, ["box", "sphere"]);
    let box_bounds = |m: &Mesh| {
        let range = m.ranges.iter().find(|r| r.part == 0).unwrap();
        let ids = &m.indices[range.first as usize..(range.first + range.count) as usize];
        Bounds::of(ids.iter().map(|i| m.vertices[*i as usize].position)).unwrap()
    };
    let b = box_bounds(&obj);
    assert!(close(b.min, [-1.5, -0.5, 1.5]) && close(b.max, [1.5, 0.5, 2.5]), "{b:?}");
    let s = outs.status(rig.id("/w")).unwrap();
    assert_eq!((s.triangles, s.parts, s.copies, s.format.as_str()), (triangles, 2, 2, "obj"), "{s:?}");
    assert!(s.bytes > 0 && s.written_seconds_ago.is_some() && s.error.is_none(), "{s:?}");

    for (name, parts) in [("scene.stl", 1), ("scene.glb", 2)] {
        rig.edit(&[set("/w.file", name.into())]);
        let m = written(&mut rig, &outs, &dir.join(name), &[]);
        assert_eq!((m.triangle_count(), m.parts.len()), (triangles, parts), "{name}");
        let b = if parts == 1 { m.bounds().unwrap() } else { box_bounds(&m) };
        assert!(close(b.max, [1.5, 0.5, 2.5]), "{name}: upright and moved: {b:?}");
        assert_eq!(outs.status(rig.id("/w")).unwrap().format, name[6..], "auto by extension");
    }
    // A move is written again; the stale file is replaced, not numbered.
    let before = std::fs::read(dir.join("scene.glb")).unwrap();
    rig.edit(&[set("/up.translate", Value::Vec3([0.0, 0.0, 5.0]))]);
    let m = written(&mut rig, &outs, &dir.join("scene.glb"), &before);
    assert!(close(box_bounds(&m).max, [1.5, 0.5, 5.5]));
    assert!(!dir.join("scene_2.glb").exists());
}

#[test]
fn copies_from_tex_instance_are_expanded_or_skipped() {
    let dir = scratch("mesh-out-instances");
    std::fs::write(
        dir.join("position.wgsl"),
        "@fragment\nfn main(v: PnVertex) -> @location(0) vec4f {\n    \
         return select(vec4f(1.5, 0.0, 0.0, 1.0), vec4f(-1.5, 0.0, 0.0, 1.0), v.uv.x < 0.5);\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("aim.wgsl"),
        "@fragment\nfn main(v: PnVertex) -> @location(0) vec4f {\n    \
         return select(vec4f(1.0, 0.0, 0.0, 1.0), vec4f(0.0, 1.0, 0.0, 1.0), v.uv.x < 0.5);\n}\n",
    )
    .unwrap();
    let texels = |path: &str, file: &str| {
        create(
            path,
            "tex.shader",
            &[
                ("file", file.into()),
                ("resolution", "custom".into()),
                ("width", Value::Int(2)),
                ("height", Value::Int(1)),
            ],
        )
    };
    let ops = vec![
        create("/stick", "geo.primitive", &[("size", Value::Vec3([0.4, 1.2, 0.4]))]),
        texels("/position", "position.wgsl"),
        texels("/aim", "aim.wgsl"),
        create("/copies", "geo.tex_instance", &[]),
        connect("/stick", "/copies", 0),
        connect("/position", "/copies", 1),
        connect("/aim", "/copies", 2),
        create("/w", "out.mesh", &[("file", "copies.glb".into())]),
        connect("/copies", "/w", 0),
    ];
    let Some((mut rig, outs)) = rig_with_outs(&ops, dir.path()) else { return };
    let file = dir.join("copies.glb");
    let m = written(&mut rig, &outs, &file, &[]);
    assert_eq!((m.triangle_count(), m.parts.clone()), (24, vec!["box_0".to_string(), "box_1".into()]));
    let part = |k: u32| {
        let r = m.ranges.iter().find(|r| r.part == k).unwrap();
        let ids = &m.indices[r.first as usize..(r.first + r.count) as usize];
        Bounds::of(ids.iter().map(|i| m.vertices[*i as usize].position)).unwrap()
    };
    // Copy 0 at x -1.5 as it is; copy 1 at x 1.5, its long y side turned along x.
    let (a, b) = (part(0), part(1));
    assert!(close(a.min, [-1.7, -0.6, -0.2]) && close(a.max, [-1.3, 0.6, 0.2]), "{a:?}");
    assert!(close(b.min, [0.9, -0.2, -0.2]) && close(b.max, [2.1, 0.2, 0.2]), "{b:?}");
    assert_eq!(outs.status(rig.id("/w")).unwrap().copies, 2);

    // Skip: the stick once, at the origin, and a warning.
    let before = std::fs::read(&file).unwrap();
    rig.edit(&[set("/w.instances", "skip".into())]);
    let m = written(&mut rig, &outs, &file, &before);
    assert_eq!((m.triangle_count(), m.parts.clone()), (12, vec!["box".to_string()]));
    let s = rig.cooker.status(rig.id("/w")).unwrap();
    assert!(s.warning.as_deref().is_some_and(|w| w.contains("without its 2 copies")), "{s:?}");

    // Too many copies: an error, nothing written.
    rig.edit(&[
        create("/many", "tex.constant", &[("width", Value::Int(1024)), ("height", Value::Int(1024))]),
        connect("/many", "/copies", 1),
        set("/w.instances", "expand".into()),
        set("/w.file", "many.glb".into()),
    ]);
    let deadline = Instant::now() + Duration::from_secs(20);
    let error = loop {
        rig.cook("/w");
        if let Some(e) = rig.cooker.status(rig.id("/w")).unwrap().error {
            break e;
        }
        assert!(Instant::now() < deadline, "no error for too many copies");
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(error.contains("more than a file may hold") && error.contains("1048576 copies"), "{error}");
    assert!(!dir.join("many.glb").exists());
}
