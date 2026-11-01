use crate::transform::{IDENTITY, Mat4, euler, mul, transform_point};
use crate::tree::{Array, Node, Value};
use crate::write::{self, connection, cube_geometry, example_scene, object, prop3, to_bytes};
use crate::*;

fn near(a: [f64; 3], b: [f64; 3]) -> bool {
    a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-6 * (1.0 + y.abs()))
}

/// The first `byte` after the file header.
fn after_header(bytes: &[u8], byte: u8) -> usize {
    27 + bytes[27..].iter().position(|b| *b == byte).unwrap()
}

#[test]
fn the_tree_survives_a_round_trip_in_every_layout() {
    let nodes = example_scene();
    for version in [7100, 7400, 7500, 7700] {
        for compress in [false, true] {
            let bytes = to_bytes(version, &nodes, compress);
            let tree = tree::parse(&bytes, &Limits::default()).unwrap();
            assert_eq!(tree.version, version);
            assert_eq!(tree.nodes, nodes, "{version} {compress}");
        }
    }
}

#[test]
fn other_files_are_refused_with_a_reason() {
    let l = Limits::default();
    let err = |b: &[u8]| tree::parse(b, &l).unwrap_err().0;
    assert!(err(b"; FBX 6.1.0 project file\nFBXHeaderExtension:  {\n").contains("FBX 6"));
    let mut be = to_bytes(7400, &[], false);
    be[22] = 1;
    assert!(err(&be).contains("big-endian"));
    assert!(err(&to_bytes(6100, &[], false)).contains("6100"));
    assert!(err(b"solid cube").contains("not an FBX file"));
    assert!(err(&MAGIC[..10]).contains("not an FBX file"));
}

#[test]
fn every_truncation_ends_in_an_error_or_a_tree() {
    let l = Limits::default();
    for (version, compress) in [(7400, true), (7500, false)] {
        let bytes = to_bytes(version, &example_scene(), compress);
        for n in 0..bytes.len() {
            let cut = &bytes[..n];
            if let Ok(doc) = Document::parse(cut, &l) {
                let _ = Scene::from_document(doc, &l).map(|s| s.mesh_instances());
            }
        }
    }
}

#[test]
fn a_zero_end_offset_means_no_children() {
    // Some Maya versions write records whose end offset is zero.
    let mut bytes = to_bytes(7400, &[Node::new("A", vec![Value::I32(1)], Vec::new())], false);
    bytes[27..31].copy_from_slice(&0u32.to_le_bytes());
    let tree = tree::parse(&bytes, &Limits::default()).unwrap();
    assert_eq!(tree.nodes, vec![Node::new("A", vec![Value::I32(1)], Vec::new())]);
}

#[test]
fn undocumented_codes_and_bad_text_are_read() {
    // `B` and `Z` bytes, a `c` array, and a string that is not UTF-8.
    let mut bytes = to_bytes(
        7400,
        &[Node::new("A", vec![Value::Byte(7), Value::String("ab".into())], Vec::new())],
        false,
    );
    let at = bytes.windows(2).position(|w| w == b"ab").unwrap();
    bytes[at] = 0xff;
    let z = after_header(&bytes, b'B');
    bytes[z] = b'Z';
    let tree = tree::parse(&bytes, &Limits::default()).unwrap();
    assert_eq!(tree.nodes[0].values, vec![Value::Byte(7), Value::String("\u{fffd}b".into())]);
    let c = to_bytes(7400, &[Node::new("C", vec![Value::Array(Array::Byte(vec![1, 2]))], Vec::new())], true);
    assert_eq!(
        tree::parse(&c, &Limits::default()).unwrap().nodes[0].values[0],
        Value::Array(Array::Byte(vec![1, 2]))
    );
}

#[test]
fn hostile_counts_are_refused_before_allocating() {
    let l = Limits::default();
    // An array claiming 4 Gi elements in a few bytes of zlib.
    let mut bytes =
        to_bytes(7400, &[Node::new("A", vec![Value::Array(Array::F64(vec![1.0; 4]))], Vec::new())], true);
    let at = after_header(&bytes, b'd');
    bytes[at + 1..at + 5].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(tree::parse(&bytes, &l).unwrap_err().0.contains("arrays exceed"));
    // A record claiming a billion values.
    let mut bytes = to_bytes(7400, &[Node::new("A", vec![Value::I32(1)], Vec::new())], false);
    bytes[31..35].copy_from_slice(&1_000_000_000u32.to_le_bytes());
    assert!(tree::parse(&bytes, &l).is_err());
    // Values budget.
    let many = Node::new("A", vec![Value::Bool(1); 100], Vec::new());
    let small = Limits { max_values: 50, ..Limits::default() };
    assert!(tree::parse(&to_bytes(7400, &[many], false), &small).unwrap_err().0.contains("values"));
}

#[test]
fn bad_zlib_is_an_error() {
    let mut bytes = to_bytes(
        7400,
        &[Node::new("A", vec![Value::Array(Array::F64((0..64).map(f64::from).collect()))], Vec::new())],
        true,
    );
    let at = after_header(&bytes, b'd') + 13;
    for b in &mut bytes[at + 2..at + 12] {
        *b ^= 0x5a;
    }
    assert!(tree::parse(&bytes, &Limits::default()).unwrap_err().0.contains("zlib"));
}

#[test]
fn deep_nesting_is_refused() {
    let mut n = Node::new("leaf", Vec::new(), Vec::new());
    for _ in 0..200 {
        n = Node::new("n", Vec::new(), vec![n]);
    }
    let err = tree::parse(&to_bytes(7400, &[n], false), &Limits::default()).unwrap_err();
    assert!(err.0.contains("deeper"), "{err}");
}

fn scene_of(objects: Vec<Node>, connections: Vec<Node>) -> Scene {
    let nodes =
        vec![Node::new("Objects", Vec::new(), objects), Node::new("Connections", Vec::new(), connections)];
    Scene::parse(&to_bytes(7400, &nodes, false), &Limits::default()).unwrap()
}

#[test]
fn a_cycle_of_parents_is_cut_with_a_warning() {
    let m = |id: i64, name: &str| {
        object("Model", id, name, "Null", vec![prop3("Lcl Translation", "", [1.0, 0.0, 0.0])], Vec::new())
    };
    let s = scene_of(
        vec![m(1, "a"), m(2, "b"), m(3, "c")],
        vec![connection("OO", 1, 2, None), connection("OO", 2, 1, None), connection("OO", 3, 1, None)],
    );
    assert_eq!(s.hierarchy.nodes.len(), 3);
    assert!(s.warnings.iter().any(|w| w.contains("own ancestor")), "{:?}", s.warnings);
    // Parents come first, and every chain is finite.
    for (i, n) in s.hierarchy.nodes.iter().enumerate() {
        assert!(n.parent.is_none_or(|p| p < i));
    }
    // A self-loop too.
    let s = scene_of(vec![m(1, "a")], vec![connection("OO", 1, 1, None)]);
    assert_eq!(s.hierarchy.nodes[0].parent, None);
}

#[test]
fn deep_model_chains_are_refused() {
    let objects: Vec<Node> =
        (1..=100).map(|i| object("Model", i, "m", "Null", Vec::new(), Vec::new())).collect();
    let conns: Vec<Node> = (2..=100).map(|i| connection("OO", i, i - 1, None)).collect();
    let nodes = vec![Node::new("Objects", Vec::new(), objects), Node::new("Connections", Vec::new(), conns)];
    let err = Scene::parse(&to_bytes(7400, &nodes, false), &Limits::default()).unwrap_err();
    assert!(err.0.contains("deeper"), "{err}");
}

#[test]
fn bad_polygons_are_skipped_and_counted() {
    let mut g = cube_geometry(20, "g");
    let idx = g.children.iter_mut().find(|c| c.name == "PolygonVertexIndex").unwrap();
    // A quad with a corner past the points, a line, and a last polygon without its end mark.
    idx.values[0] = Value::Array(Array::I32(vec![0, 1, 99, -3, 0, -2, 0, 1, 2, 3]));
    let geo = Geometry::read(&g, &Limits::default()).unwrap();
    assert_eq!((geo.out_of_range, geo.degenerate), (1, 1));
    assert_eq!(geo.polygons, vec![Polygon { first: 6, count: 4, index: 2 }]);
    // Layers stay indexed by corner as in the file.
    assert_eq!(geo.normals.as_ref().unwrap().at(6), Some([0.0, 0.0, 1.0]));
}

#[test]
fn mapping_modes_resolve_per_corner() {
    let layer = |mapping: &str, values: Vec<f64>, index: Option<Vec<i32>>| {
        let mut children = vec![
            write::leaf("MappingInformationType", Value::String(mapping.into())),
            write::leaf("Normals", Value::Array(Array::F64(values))),
        ];
        if let Some(i) = index {
            children.push(write::leaf("NormalsIndex", Value::Array(Array::I32(i))));
        }
        Node::new("LayerElementNormal", vec![Value::I32(0)], children)
    };
    let geo = |l: Node| {
        let mut g = object("Geometry", 1, "g", "Mesh", Vec::new(), Vec::new());
        g.children.push(write::leaf("Vertices", Value::Array(Array::F64(vec![0.0; 12]))));
        // Two triangles over four points.
        g.children
            .push(write::leaf("PolygonVertexIndex", Value::Array(Array::I32(vec![0, 1, -3, 0, 2, -4]))));
        g.children.push(l);
        Geometry::read(&g, &Limits::default()).unwrap().normals.unwrap()
    };
    let x = |k: f64| vec![k, 0.0, 0.0];
    let four: Vec<f64> = (0..4).flat_map(|k| x(f64::from(k))).collect();
    let firsts = |a: &Attribute<[f64; 3]>| (0..6).map(|c| a.at(c).map(|v| v[0])).collect::<Vec<_>>();
    // By control point, direct: corner -> its point.
    assert_eq!(
        firsts(&geo(layer("ByVertice", four.clone(), None))),
        [0.0, 1.0, 2.0, 0.0, 2.0, 3.0].map(Some)
    );
    // By polygon, through an index.
    let two: Vec<f64> = [x(5.0), x(6.0)].concat();
    assert_eq!(
        firsts(&geo(layer("ByPolygon", two, Some(vec![1, 0])))),
        [6.0, 6.0, 6.0, 5.0, 5.0, 5.0].map(Some)
    );
    // `ByPolygon` with one entry per corner means per corner (old writers).
    let six: Vec<f64> = (0..6).flat_map(|k| x(f64::from(k))).collect();
    assert_eq!(firsts(&geo(layer("ByPolygon", six, None))), [0.0, 1.0, 2.0, 3.0, 4.0, 5.0].map(Some));
    // All the same; and an index out of range is none.
    assert_eq!(firsts(&geo(layer("AllSame", x(9.0), None))), [Some(9.0); 6]);
    let bad = geo(layer("ByPolygonVertex", four, Some(vec![0, 1, 2, 3, 7, -1])));
    assert_eq!(firsts(&bad), [Some(0.0), Some(1.0), Some(2.0), Some(3.0), None, None]);
}

#[test]
fn concave_polygons_triangulate_inside() {
    // An L of six corners in the xy plane, counter-clockwise.
    let l = [
        [0.0, 0.0, 0.0],
        [2.0, 0.0, 0.0],
        [2.0, 1.0, 0.0],
        [1.0, 1.0, 0.0],
        [1.0, 2.0, 0.0],
        [0.0, 2.0, 0.0],
    ];
    let tris = geometry::triangulate(&l);
    assert_eq!(tris.len(), 4);
    let area: f64 = tris
        .iter()
        .map(|t| {
            let [a, b, c] = t.map(|i| l[i]);
            0.5 * ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]))
        })
        .sum();
    // All counter-clockwise (positive), summing to the L's area: none outside it.
    assert!((area - 3.0).abs() < 1e-9, "{area}");
    // The same seen from behind keeps its winding (clockwise in xy).
    let back: Vec<[f64; 3]> = l.iter().rev().copied().collect();
    assert_eq!(geometry::triangulate(&back).len(), 4);
}

/// The full formula, matrix by matrix.
fn reference(p: &TransformProps) -> Mat4 {
    let t = |v: [f64; 3]| {
        let mut m = IDENTITY;
        m[0][3] = v[0];
        m[1][3] = v[1];
        m[2][3] = v[2];
        m
    };
    let neg = |v: [f64; 3]| v.map(|x| -x);
    let r = |m3: [[f64; 3]; 3]| {
        let mut m = IDENTITY;
        for i in 0..3 {
            for j in 0..3 {
                m[i][j] = m3[i][j];
            }
        }
        m
    };
    let s = |v: [f64; 3]| {
        let mut m = IDENTITY;
        m[0][0] = v[0];
        m[1][1] = v[1];
        m[2][2] = v[2];
        m
    };
    let post = euler(p.post_rotation, RotationOrder::Xyz);
    let post_inv = [0, 1, 2].map(|i| [0, 1, 2].map(|j| post[j][i]));
    let chain = [
        t(p.translation),
        t(p.rotation_offset),
        t(p.rotation_pivot),
        r(euler(p.pre_rotation, RotationOrder::Xyz)),
        r(euler(p.rotation, p.rotation_order)),
        r(post_inv),
        t(neg(p.rotation_pivot)),
        t(p.scaling_offset),
        t(p.scaling_pivot),
        s(p.scaling),
        t(neg(p.scaling_pivot)),
    ];
    chain.iter().fold(IDENTITY, |acc, m| mul(&acc, m))
}

#[test]
fn the_local_transform_matches_the_full_formula() {
    for order in 0..6 {
        let p = TransformProps {
            translation: [1.0, -2.0, 3.0],
            rotation: [10.0, 70.0, -35.0],
            scaling: [2.0, 0.5, 1.5],
            pre_rotation: [-90.0, 0.0, 15.0],
            post_rotation: [5.0, 25.0, -60.0],
            rotation_offset: [0.3, 0.2, 0.1],
            rotation_pivot: [-1.0, 0.5, 2.0],
            scaling_offset: [0.1, -0.4, 0.0],
            scaling_pivot: [0.7, 0.0, -0.3],
            rotation_order: RotationOrder::from_index(order),
            ..TransformProps::default()
        };
        let got = p.local().matrix();
        let want = reference(&p);
        for (a, b) in got.iter().zip(&want) {
            assert!(a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9), "order {order}: {got:?} vs {want:?}");
        }
    }
    // Rotation order: XYZ turns about x first. 90 about x then 90 about z takes +y to +z, then
    // +z stays.
    let p = TransformProps { rotation: [90.0, 0.0, 90.0], ..TransformProps::default() };
    assert!(near(transform_point(&p.local().matrix(), [0.0, 1.0, 0.0]), [0.0, 0.0, 1.0]));
    // ZYX: about z first: +y to -x, then x by 90 keeps -x.
    let p = TransformProps { rotation_order: RotationOrder::Zyx, ..p };
    assert!(near(transform_point(&p.local().matrix(), [0.0, 1.0, 0.0]), [-1.0, 0.0, 0.0]));
    // Without RotationActive, pre-rotation and the order do not count.
    let p = TransformProps { pre_rotation: [0.0, 0.0, 90.0], rotation_space: false, ..p };
    assert!(near(transform_point(&p.local().matrix(), [0.0, 1.0, 0.0]), [0.0, 0.0, 1.0]));
}

#[test]
fn the_example_scene_lands_where_it_should() {
    let bytes = to_bytes(7400, &example_scene(), true);
    let s = Scene::parse(&bytes, &Limits::default()).unwrap();
    assert_eq!(s.unit_meters, 0.01);
    assert_eq!(s.axes, Some(Axes::Y_UP));
    let inst = s.mesh_instances();
    assert_eq!(inst.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), ["base", "top"]);
    // The base cube: 1 m, its bottom at y = 0.
    let base = &inst[0];
    assert!(near(transform_point(&base.geometry_to_world, [0.0, -0.5, 0.0]), [0.0, 0.0, 0.0]));
    // The top cube's centre: see `example_scene`.
    let c = transform_point(&inst[1].geometry_to_world, [0.0; 3]);
    let h = 25.0 * std::f64::consts::FRAC_1_SQRT_2;
    assert!(near(c, [-h, 150.0, h]), "{c:?}");
    // The geometric offset does not reach a child: the model itself sits at y = 150 - 0.
    let w = s.worlds[s.hierarchy.index_of(11).unwrap()].world;
    assert!(near(transform_point(&w, [0.0; 3]), [0.0, 150.0, 0.0]));
    // The template's RotationActive applies (else the pre-rotation would not count).
    assert!(s.hierarchy.nodes[1].props.rotation_space);
    let m = s.material(30).unwrap();
    assert_eq!((m.name.as_str(), m.diffuse), ("red", [0.8, 0.1, 0.1]));
    let g = s.geometry(20, &Limits::default()).unwrap();
    assert_eq!(
        (g.positions.len(), g.polygons.len(), g.materials, g.uv_set.as_str()),
        (8, 6, vec![0; 6], "map1")
    );
}

#[test]
fn inherit_types_follow_their_parents_scale_or_not() {
    let m = |id: i64, t: [f64; 3], s: [f64; 3], r: [f64; 3], inherit: i32| {
        object(
            "Model",
            id,
            "m",
            "Null",
            vec![
                prop3("Lcl Translation", "", t),
                prop3("Lcl Scaling", "", s),
                prop3("Lcl Rotation", "", r),
                write::prop("InheritType", "enum", vec![Value::I32(inherit)]),
            ],
            Vec::new(),
        )
    };
    let run = |inherit: i32| {
        let s = scene_of(
            vec![
                m(1, [0.0; 3], [2.0, 1.0, 1.0], [0.0; 3], 1),
                m(2, [1.0, 0.0, 0.0], [1.0; 3], [0.0, 0.0, 90.0], inherit),
            ],
            vec![connection("OO", 1, 0, None), connection("OO", 2, 1, None)],
        );
        let w = s.worlds[s.hierarchy.index_of(2).unwrap()].world;
        (
            transform_point(&w, [0.0; 3]),
            transform_point(&w, [1.0, 0.0, 0.0]),
            transform_point(&w, [0.0, 1.0, 0.0]),
        )
    };
    // Normal: the child at x = 2, its x axis (turned to y) not stretched, its y (turned to -x)
    // stretched by the parent's 2 in x.
    let (o, x, y) = run(1);
    assert!(near(o, [2.0, 0.0, 0.0]) && near(x, [2.0, 1.0, 0.0]) && near(y, [0.0; 3]), "{o:?} {x:?} {y:?}");
    // RrSs: the parent's scale applies along the child's own axes: its x doubled.
    let (o, x, y) = run(0);
    assert!(
        near(o, [2.0, 0.0, 0.0]) && near(x, [2.0, 2.0, 0.0]) && near(y, [1.0, 0.0, 0.0]),
        "{o:?} {x:?} {y:?}"
    );
    // Rrs: the parent's scale does not reach the child's size; its offset still is scaled.
    let (o, x, y) = run(2);
    assert!(
        near(o, [2.0, 0.0, 0.0]) && near(x, [2.0, 1.0, 0.0]) && near(y, [1.0, 0.0, 0.0]),
        "{o:?} {x:?} {y:?}"
    );
}

#[test]
fn axes_convert_to_any_target() {
    let max = Axes { right: Axis::PosX, up: Axis::PosZ, front: Axis::NegY };
    let m = Axes::Y_UP.conversion_to(&max);
    assert!(near(transform_point(&m, [0.0, 1.0, 0.0]), [0.0, 0.0, 1.0]));
    assert!(near(transform_point(&m, [0.0, 0.0, 1.0]), [0.0, -1.0, 0.0]));
    assert!(near(transform_point(&max.conversion_to(&max), [1.0, 2.0, 3.0]), [1.0, 2.0, 3.0]));
}

fn curve_of(seconds: &[f64], values: &[f32], flags: i32) -> anim::Curve {
    anim::Curve::read(&write::curve(1, seconds, values, flags), anim::TICKS_PER_SECOND as f64).unwrap()
}

#[test]
fn curves_interpolate_as_their_keys_say() {
    let linear = curve_of(&[0.0, 1.0, 3.0], &[0.0, 10.0, 0.0], write::KEY_LINEAR);
    assert!((linear.evaluate(0.5, 0.0) - 5.0).abs() < 1e-9);
    assert!((linear.evaluate(2.0, 0.0) - 5.0).abs() < 1e-9);
    // Constant extrapolation on both ends.
    assert_eq!(linear.evaluate(-1.0, 0.0), 0.0);
    assert_eq!(linear.evaluate(9.0, 0.0), 0.0);
    let held = curve_of(&[0.0, 1.0], &[2.0, 4.0], write::KEY_CONSTANT);
    assert_eq!(held.evaluate(0.99, 0.0), 2.0);
    assert_eq!(held.evaluate(1.0, 0.0), 4.0);
    let next = curve_of(&[0.0, 1.0], &[2.0, 4.0], write::KEY_CONSTANT | 0x100);
    assert_eq!(next.evaluate(0.01, 0.0), 4.0);
    // Two cubic keys with auto tangents: each end follows the chord, a straight line.
    let auto_ends = curve_of(&[0.0, 1.0], &[0.0, 1.0], write::KEY_CUBIC);
    assert!((auto_ends.evaluate(0.1, 0.0) - 0.1).abs() < 1e-6);
    // Flat user tangents ease in and out: symmetric, slow at the ends.
    let eased = curve_of(&[0.0, 1.0], &[0.0, 1.0], write::KEY_CUBIC | 0x400);
    assert!((eased.evaluate(0.5, 0.0) - 0.5).abs() < 1e-6);
    assert!(eased.evaluate(0.1, 0.0) < 0.05 && eased.evaluate(0.9, 0.0) > 0.95);
    // Three cubic keys: the middle one gets a slope from its neighbours, so the curve passes
    // through it rising.
    let auto = curve_of(&[0.0, 1.0, 2.0], &[0.0, 1.0, 2.0], write::KEY_CUBIC);
    assert!((auto.evaluate(1.0, 0.0) - 1.0).abs() < 1e-9);
    assert!(auto.evaluate(1.1, 0.0) > 1.05);
    // No keys: the default.
    let none = anim::Curve::default();
    assert_eq!(none.evaluate(1.0, 7.0), 7.0);
}

#[test]
fn curves_repeat_mirror_and_slope_beyond_their_keys() {
    let mut c = curve_of(&[0.0, 1.0], &[0.0, 10.0], write::KEY_LINEAR);
    let at = |c: &anim::Curve, t: f64| c.evaluate(t, 0.0);
    c.post = anim::Beyond { mode: anim::Extrapolation::Repeat, repetitions: None };
    assert!((at(&c, 1.25) - 2.5).abs() < 1e-6);
    c.post.mode = anim::Extrapolation::RepeatRelative;
    assert!((at(&c, 1.25) - 12.5).abs() < 1e-6);
    c.post.mode = anim::Extrapolation::Mirror;
    assert!((at(&c, 1.25) - 7.5).abs() < 1e-6);
    c.post = anim::Beyond { mode: anim::Extrapolation::Repeat, repetitions: Some(1) };
    assert!((at(&c, 5.5) - 10.0).abs() < 1e-6, "one repetition, then the end holds");
    c.pre = anim::Beyond { mode: anim::Extrapolation::Slope, repetitions: None };
    assert!((at(&c, -0.5) + 5.0).abs() < 1e-4);
}

#[test]
fn euler_angles_come_back_from_their_matrix_in_every_order() {
    use crate::transform::RotationOrder::*;
    for order in [Xyz, Xzy, Yzx, Yxz, Zxy, Zyx] {
        for angles in [[10.0, 20.0, 30.0], [-170.0, 45.0, 99.0], [30.0, 90.0, 10.0], [5.0, -90.0, 40.0]] {
            let m = euler(angles, order);
            let back = euler(anim::euler_of(&m, order), order);
            for (r, s) in m.iter().zip(&back) {
                for (a, b) in r.iter().zip(s) {
                    assert!((a - b).abs() < 1e-9, "{order:?} {angles:?}");
                }
            }
        }
    }
}

#[test]
fn the_animated_scene_moves_its_models() {
    let bytes = to_bytes(7400, &write::animated_scene(), false);
    let scene = Scene::parse(&bytes, &Limits::default()).unwrap();
    let a = &scene.animation;
    assert_eq!(a.takes.len(), 1);
    assert_eq!(a.frame_rate, 30.0);
    let take = &a.takes[0];
    assert_eq!((take.name.as_str(), take.start, take.stop), ("Take 001", 0.0, 2.0));
    assert!(a.moves(&scene.hierarchy, 0));
    let base = scene.hierarchy.index_of(10).unwrap();
    let top = scene.hierarchy.index_of(11).unwrap();
    let w = a.worlds(&scene.hierarchy, 0, 1.0);
    // Halfway: base at 100 cm.
    assert!(near(transform_point(&w[base].world, [0.0; 3]), [0.0, 100.0, 0.0]));
    // The take's start is the file's pose.
    let w0 = a.worlds(&scene.hierarchy, 0, 0.0);
    assert!(w0.iter().zip(&scene.worlds).all(|(x, y)| x.world == y.world));
    // At the end, top has turned 90 degrees about z (after its pre-rotation of 90).
    let p = a.transform_props(&scene.hierarchy, 0, 2.0);
    assert!(near(p[top].rotation, [0.0, 0.0, 90.0]));
    // A take that does not exist leaves the file's values.
    assert_eq!(a.transform_props(&scene.hierarchy, 5, 1.0)[base], scene.hierarchy.nodes[base].props);
}

#[test]
fn animation_budgets_and_broken_curves() {
    let bytes = to_bytes(7400, &write::animated_scene(), true);
    let few = Limits { max_keys: 3, ..Limits::default() };
    assert!(Scene::parse(&bytes, &few).unwrap_err().0.contains("animation keys"));
    let one = Limits { max_curves: 1, ..Limits::default() };
    assert!(Scene::parse(&bytes, &one).unwrap_err().0.contains("animation curves"));
    // A curve whose arrays disagree is left out with a warning; the rest still moves.
    let mut nodes = write::animated_scene();
    for n in &mut nodes {
        if n.name == "Objects"
            && let Some(c) = n.children.iter_mut().find(|c| c.value(0) == Some(&Value::I64(45)))
            && let Some(v) = c.children.iter_mut().find(|v| v.name == "KeyValueFloat")
        {
            v.values = vec![Value::Array(Array::F32(vec![1.0]))];
        }
    }
    let scene = Scene::parse(&to_bytes(7400, &nodes, false), &Limits::default()).unwrap();
    assert!(scene.warnings.iter().any(|w| w.contains("curves could not be read")));
    assert_eq!(scene.animation.curves.len(), 1);
    // Every truncation of an animated file ends in an error or a scene, and evaluating it at
    // any time is safe.
    for n in (0..bytes.len()).step_by(7) {
        if let Ok(s) = Scene::parse(&bytes[..n], &Limits::default()) {
            for t in [-1.0, 0.5, f64::NAN, 1e300] {
                let _ = s.animation.worlds(&s.hierarchy, 0, t);
            }
        }
    }
}

/// The skinned scene's skin, and its geometry's control points in file space.
fn skinned() -> (Scene, Skin, Vec<[f64; 3]>) {
    let scene = Scene::parse(&to_bytes(7400, &write::skinned_scene(), true), &Limits::default()).unwrap();
    let skin = scene.skin(20, &Limits::default()).unwrap().expect("a skin");
    let g = scene.geometry(20, &Limits::default()).unwrap();
    let points = g.positions.clone();
    (scene, skin, points)
}

#[test]
fn skins_move_their_points_with_the_bones() {
    let (scene, skin, points) = skinned();
    assert!(skin.warnings.is_empty(), "{:?}", skin.warnings);
    assert_eq!(skin.method, SkinningMethod::Linear);
    assert_eq!(skin.points(), 8);
    assert_eq!(skin.clusters.len(), 2);
    assert_eq!(skin.max_influences(), 2);
    // Point 7 has both bones, the heavier first, as stored.
    let w: Vec<(u32, f64)> = skin.influences(7).iter().map(|i| (i.cluster, i.weight)).collect();
    assert_eq!(w, vec![(1, 1.5), (0, 0.5)]);
    // The hip cluster's bind matrix came from the bind pose, its Transform from that.
    let hip = &skin.clusters[0];
    assert!(near([hip.bind_to_world[1][3], 0.0, 0.0], [-100.0, 0.0, 0.0]));
    let body = scene.hierarchy.index_of(10).unwrap();
    // At bind time every point sits where the body places it.
    let rest = scene.worlds[body].geometry;
    let mats = skin.cluster_matrices(&scene.worlds);
    for (p, q) in points.iter().enumerate() {
        let m = skin.point_matrix(p, &mats).unwrap();
        assert!(near(transform_point(&m, *q), transform_point(&rest, *q)), "point {p}");
    }
    // Two seconds in, the knee has turned 90 degrees about z: the top points swing over to
    // x -100; point 7 goes a quarter with the hip and three quarters with the knee.
    let worlds = scene.animation.worlds(&scene.hierarchy, 0, 2.0);
    let mats = skin.cluster_matrices(&worlds);
    let at = |p: usize| transform_point(&skin.point_matrix(p, &mats).unwrap(), points[p]);
    assert!(near(at(0), [-50.0, -100.0, -50.0]));
    assert!(near(at(2), [-100.0, -50.0, -50.0]));
    assert!(near(at(7), [-62.5, 62.5, 50.0]), "{:?}", at(7));
}

#[test]
fn broken_skins_are_read_safely() {
    let edit = |f: &dyn Fn(&mut Node)| {
        let mut nodes = write::skinned_scene();
        for n in nodes.iter_mut().filter(|n| n.name == "Objects") {
            for c in &mut n.children {
                f(c);
            }
        }
        Scene::parse(&to_bytes(7400, &nodes, false), &Limits::default()).unwrap()
    };
    // Weights and indexes of different lengths: that cluster's weights are left out.
    let scene = edit(&|c| {
        if c.value(0) == Some(&Value::I64(52))
            && let Some(w) = c.children.iter_mut().find(|w| w.name == "Weights")
        {
            w.values = vec![Value::Array(Array::F64(vec![1.0]))];
        }
    });
    let skin = scene.skin(20, &Limits::default()).unwrap().unwrap();
    assert!(skin.warnings.iter().any(|w| w.contains("as many weights")));
    assert!(skin.influences(2).is_empty());
    let mats = skin.cluster_matrices(&scene.worlds);
    assert!(skin.point_matrix(2, &mats).is_none());
    // Indexes beyond the geometry and negative ones are dropped.
    let scene = edit(&|c| {
        if c.value(0) == Some(&Value::I64(51))
            && let Some(i) = c.children.iter_mut().find(|i| i.name == "Indexes")
        {
            i.values = vec![Value::Array(Array::I32(vec![0, -1, 8, 1 << 30, 7]))];
        }
    });
    let skin = scene.skin(20, &Limits::default()).unwrap().unwrap();
    assert_eq!(skin.influences(0).len(), 1);
    assert_eq!(skin.influences(7).len(), 2);
    // Dual quaternion skins are blended linearly, with a warning.
    let scene = edit(&|c| {
        if let Some(t) = c.children.iter_mut().find(|t| t.name == "SkinningType") {
            t.values = vec![Value::String("DualQuaternion".into())];
        }
    });
    let skin = scene.skin(20, &Limits::default()).unwrap().unwrap();
    assert!(skin.dual_quaternion);
    assert!(skin.warnings.iter().any(|w| w.contains("dual quaternion")));
    // The weight budget.
    let bytes = to_bytes(7400, &write::skinned_scene(), false);
    let scene = Scene::parse(&bytes, &Limits::default()).unwrap();
    let few = Limits { max_skin_weights: 6, ..Limits::default() };
    assert!(scene.skin(20, &few).unwrap_err().0.contains("weights"));
    // Every truncation: an error or a scene whose skin reads and poses at any time.
    for n in (0..bytes.len()).step_by(5) {
        if let Ok(s) = Scene::parse(&bytes[..n], &Limits::default())
            && let Ok(Some(skin)) = s.skin(20, &Limits::default())
        {
            for t in [-1.0, 0.5, f64::NAN, 1e300] {
                let mats = skin.cluster_matrices(&s.animation.worlds(&s.hierarchy, 0, t));
                for p in 0..skin.points() + 2 {
                    let _ = skin.point_matrix(p, &mats);
                }
            }
        }
    }
}

/// Whether two values hold the same thing, whatever type each form chose: numbers as numbers,
/// arrays element by element (as `f32` when either side is), binary object names against
/// `Class::Name`.
fn same_value(a: &Value, b: &Value) -> bool {
    let num = |x: f64, y: f64| x == y || (x.is_nan() && y.is_nan());
    match (a, b) {
        (Value::String(x), Value::String(y)) => {
            document::split_name(x) == document::split_name(y)
                && (x.contains("\u{0}\u{1}") || y.contains("\u{0}\u{1}") || x == y)
        }
        (Value::Raw(x), Value::Raw(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => {
            let single = matches!(x, Array::F32(_)) || matches!(y, Array::F32(_));
            x.len() == y.len()
                && (0..x.len()).all(|i| {
                    let (p, q) = (x.f64_at(i).unwrap(), y.f64_at(i).unwrap());
                    if single { num(f64::from(p as f32), f64::from(q as f32)) } else { num(p, q) }
                })
        }
        (x, y) => match (x.as_f64(), y.as_f64()) {
            (Some(p), Some(q)) => num(p, q),
            _ => false,
        },
    }
}

fn same_node(a: &Node, b: &Node) -> bool {
    a.name == b.name
        && a.values.len() == b.values.len()
        && a.values.iter().zip(&b.values).all(|(x, y)| same_value(x, y))
        && a.children.len() == b.children.len()
        && a.children.iter().zip(&b.children).all(|(x, y)| same_node(x, y))
}

fn world_matrices(w: &[NodeWorld]) -> Vec<Mat4> {
    w.iter().map(|w| w.world).collect()
}

#[test]
fn ascii_files_read_as_their_binary_twins() {
    let l = Limits::default();
    let scenes = [example_scene(), write::animated_scene(), write::skinned_scene()];
    for (k, nodes) in scenes.iter().enumerate() {
        let binary = to_bytes(7400, nodes, false);
        let ascii = write::to_ascii(7400, nodes);
        let (bt, at) = (tree::parse(&binary, &l).unwrap(), tree::parse(ascii.as_bytes(), &l).unwrap());
        assert_eq!(at.version, 7400);
        assert_eq!(bt.nodes.len(), at.nodes.len());
        for (x, y) in bt.nodes.iter().zip(&at.nodes) {
            assert!(same_node(x, y), "scene {k}: `{}` differs\n{x:?}\n{y:?}", x.name);
        }
        let (b, a) = (Scene::parse(&binary, &l).unwrap(), Scene::parse(ascii.as_bytes(), &l).unwrap());
        assert_eq!(world_matrices(&b.worlds), world_matrices(&a.worlds));
        let (bi, ai) = (b.mesh_instances(), a.mesh_instances());
        assert_eq!(bi.len(), ai.len());
        for (x, y) in bi.iter().zip(&ai) {
            assert_eq!((x.name.as_str(), x.geometry_to_world), (y.name.as_str(), y.geometry_to_world));
            let (gx, gy) = (b.geometry(x.geometry_id, &l).unwrap(), a.geometry(y.geometry_id, &l).unwrap());
            assert_eq!((gx.positions, gx.corners), (gy.positions, gy.corners));
            let (sx, sy) = (b.skin(x.geometry_id, &l).unwrap(), a.skin(y.geometry_id, &l).unwrap());
            assert_eq!(sx.is_some(), sy.is_some());
            if let (Some(sx), Some(sy)) = (sx, sy) {
                assert_eq!(sx.cluster_matrices(&b.worlds), sy.cluster_matrices(&a.worlds));
            }
        }
        assert_eq!(b.animation.takes.len(), a.animation.takes.len());
        for t in [0.0, 0.4, 1.0, 1.7, 2.0] {
            let (wb, wa) = (b.animation.worlds(&b.hierarchy, 0, t), a.animation.worlds(&a.hierarchy, 0, t));
            assert_eq!(world_matrices(&wb), world_matrices(&wa), "scene {k} at {t}");
        }
    }
}

#[test]
fn ascii_syntax_reads_as_the_sdk_writes_it() {
    let text = "\u{feff}; FBX 7.5.0 project file\n\
        ; a comment, with \"quotes\" and { braces }\n\
        Top:  { ; trailing comment\n\
        \tName: \"Model::a &quot;b&quot;\", \"x&ly&lf;z\", 12, -3.5e2, Y, word\n\
        \tVertices: *3 {\n\t\ta: 1,2.5,-3\n\t}\n\
        \tPolygonVertexIndex: *4 { a: 0,1,-3, 3000000000 }\n\
        \tKeyValueFloat: *2 { a: 218434821,0.1 }\n\
        \tKeyAttrDataFloat: *2 { a: 1065353216,-1082130432 }\n\
        \tEmpty: *0 { a: }\n\
        \tIndexes: 4, 5, 6\n\
        \tContent: , \"aGVs\", \"bG8=\"\n\
        \tOdd: 1.#INF, -1.#IND, nan\n\
        \tWords: *4 { a: inf, -Infinity, nan(ind), snan }\n\
        \t\"Spaced Name\": 1\n\
        }\n";
    let tree = tree::parse(text.as_bytes(), &Limits::default()).unwrap();
    // No `FBXVersion`: the comment says it.
    assert_eq!(tree.version, 7500);
    let top = tree.top("Top").unwrap();
    let name = top.child("Name").unwrap();
    assert_eq!(name.values[0], Value::String("Model::a \"b\"".into()));
    assert_eq!(document::split_name(name.values[0].as_str().unwrap()), ("a \"b\"", "Model"));
    assert_eq!(name.values[1], Value::String("x&ly\nz".into()));
    assert_eq!(
        &name.values[2..],
        &[Value::I64(12), Value::F64(-350.0), Value::Bool(b'Y'), Value::String("word".into())]
    );
    assert_eq!(top.child_array("Vertices"), Some(&Array::F64(vec![1.0, 2.5, -3.0])));
    assert_eq!(top.child_array("PolygonVertexIndex"), Some(&Array::I64(vec![0, 1, -3, 3_000_000_000])));
    assert_eq!(top.child_array("KeyValueFloat"), Some(&Array::F32(vec![218434821.0, 0.1])));
    // From 7200 on, the integers of the floats' bits; before, the floats themselves.
    assert_eq!(top.child_array("KeyAttrDataFloat"), Some(&Array::F32(vec![1.0, -1.0])));
    let old = tree::parse(
        b"; FBX 7.1.0 project file\nK: { KeyAttrDataFloat: *1 { a: 0.25 } }\n",
        &Limits::default(),
    );
    assert_eq!(old.unwrap().nodes[0].child_array("KeyAttrDataFloat"), Some(&Array::F32(vec![0.25])));
    assert_eq!(top.child_array("Empty"), Some(&Array::I32(Vec::new())));
    assert_eq!(top.child_array("Indexes"), Some(&Array::I32(vec![4, 5, 6])));
    assert_eq!(top.child("Content").unwrap().values, [Value::Raw(b"hello".to_vec())]);
    let odd: Vec<f64> = top.child("Odd").unwrap().values.iter().map(|v| v.as_f64().unwrap()).collect();
    assert!(odd[0] == f64::INFINITY && odd[1].is_nan() && odd[2].is_nan(), "{odd:?}");
    assert_eq!(top.child("Spaced Name").unwrap().values, [Value::I64(1)]);
    let words = top.child_array("Words").unwrap();
    assert!(words.f64_at(0) == Some(f64::INFINITY) && words.f64_at(1) == Some(f64::NEG_INFINITY));
    assert!(words.f64_at(2).unwrap().is_nan() && words.f64_at(3) == Some(f64::from(b's')));
    // `FBXVersion` wins over the comment; an embedded file survives the trip.
    let nodes = vec![
        Node::new("FBXHeaderExtension", Vec::new(), vec![write::leaf("FBXVersion", Value::I32(7700))]),
        write::leaf("Content", Value::Raw((0..=255).collect())),
    ];
    let back = tree::parse(write::to_ascii(7300, &nodes).as_bytes(), &Limits::default()).unwrap();
    assert_eq!(back.version, 7700);
    assert_eq!(back.nodes[1].values, [Value::Raw((0..=255).collect())]);
}

#[test]
fn hostile_ascii_is_refused_with_a_reason() {
    let l = Limits::default();
    let head = "; FBX 7.4.0 project file\n";
    let err = |body: &str| tree::parse(format!("{head}{body}").as_bytes(), &l).unwrap_err().0;
    // A claimed length the file does not back costs nothing and is not trusted.
    let t = tree::parse(format!("{head}A: *18446744073709551615 {{ a: 1,2 }}\n").as_bytes(), &l).unwrap();
    assert_eq!(t.nodes[0].values, [Value::Array(Array::I32(vec![1, 2]))]);
    assert!(err("A: *99999999999999999999 { a: 1 }").contains("no length"));
    assert!(err("A: \"open\nB: 1\n").contains("closing quote"));
    assert!(err("A: {\n B: 1\n").contains("not closed"));
    assert!(err("A: 1\n}\n").contains("never opened"));
    assert!(err("A: *2 { a: 1, \"x\" }").contains("where a number"));
    assert!(err("A: 1.2.3").contains("not a number"));
    assert!(err("A: 1.#NOPE").contains("not a number"));
    assert!(err("A: *2 { a: 1, 2e }").contains("not a number"));
    assert!(err("A: 1, @").contains("unexpected character `@`"));
    assert!(err("A: 1\nB: 2, 3 x\n").contains("expected a record name"));
    assert!(err("A: 1\n\nB: 2,\n\u{1}").contains("line 5"));
    let deep = "A: {".repeat(1000);
    assert!(err(&deep).contains("deeper than"));
    // Versions: FBX 6 by comment or by record, none at all, the future.
    assert!(tree::parse(b"; FBX 6.1.0 project file\nA: 1\n", &l).unwrap_err().0.contains("FBX 6"));
    let v = |n: i64| format!("FBXHeaderExtension: {{\n FBXVersion: {n}\n}}\n");
    assert!(tree::parse(v(6100).as_bytes(), &l).unwrap_err().0.contains("FBX 6"));
    assert!(tree::parse(v(12000).as_bytes(), &l).unwrap_err().0.contains("not known"));
    assert!(tree::parse(v(-1).as_bytes(), &l).unwrap_err().0.contains("not known"));
    assert!(tree::parse(b"A: 1\n", &l).unwrap_err().0.contains("not an FBX file"));
    // Budgets count values and elements, not bytes.
    let small = Limits { max_array_bytes: 80, ..Limits::default() };
    let many = format!("{head}A: *11 {{ a: {} }}\n", ["1"; 11].join(","));
    assert!(tree::parse(many.as_bytes(), &small).unwrap_err().0.contains("arrays exceed"));
    let few = Limits { max_values: 3, ..Limits::default() };
    assert!(
        tree::parse(format!("{head}A: 1,2\nB: 3,4\n").as_bytes(), &few).unwrap_err().0.contains("values")
    );
    let records = Limits { max_nodes: 2, ..Limits::default() };
    assert!(
        tree::parse(format!("{head}A: 1\nB: 2\nC: 3\n").as_bytes(), &records)
            .unwrap_err()
            .0
            .contains("records")
    );
    // Every truncation of each scene ends in an error or a scene that evaluates safely.
    for nodes in [example_scene(), write::animated_scene(), write::skinned_scene()] {
        let text = write::to_ascii(7400, &nodes);
        let bytes = text.as_bytes();
        for n in (0..bytes.len()).step_by(3) {
            if let Ok(s) = Scene::parse(&bytes[..n], &l) {
                let _ = s.mesh_instances();
                for t in [-1.0, 0.5, f64::NAN, 1e300] {
                    let _ = s.animation.worlds(&s.hierarchy, 0, t);
                }
            }
        }
    }
}
