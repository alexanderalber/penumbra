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
    assert!(err(b"; FBX 7.4.0 project file\nFBXHeaderExtension:  {\n").contains("ASCII"));
    let mut be = to_bytes(7400, &[], false);
    be[22] = 1;
    assert!(err(&be).contains("big-endian"));
    assert!(err(&to_bytes(6100, &[], false)).contains("6100"));
    assert!(err(b"solid cube").contains("not a binary FBX"));
    assert!(err(&MAGIC[..10]).contains("not a binary FBX"));
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
