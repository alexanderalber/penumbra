//! Writes a node tree as a binary FBX file, plus helpers to build the records of a scene.
//! Meant for tests and tools that need small files: no footer is written, and nothing checks
//! that the tree is a valid scene.

use crate::MAGIC;
use crate::tree::{Array, Node, Value};

/// The bytes of a binary FBX file of `version` (7000 or later) holding `nodes`. With
/// `compress`, arrays are stored zlib-compressed.
pub fn to_bytes(version: u32, nodes: &[Node], compress: bool) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    out.push(0);
    out.extend(version.to_le_bytes());
    let wide = version >= 7500;
    for n in nodes {
        record(&mut out, n, wide, compress);
    }
    null(&mut out, wide);
    out
}

fn null(out: &mut Vec<u8>, wide: bool) {
    out.extend(std::iter::repeat_n(0u8, if wide { 25 } else { 13 }));
}

fn number(out: &mut [u8], at: usize, v: usize, wide: bool) {
    if wide {
        if let Some(s) = out.get_mut(at..at + 8) {
            s.copy_from_slice(&(v as u64).to_le_bytes());
        }
    } else if let Some(s) = out.get_mut(at..at + 4) {
        s.copy_from_slice(&(v as u32).to_le_bytes());
    }
}

fn record(out: &mut Vec<u8>, n: &Node, wide: bool, compress: bool) {
    let start = out.len();
    let w = if wide { 8 } else { 4 };
    out.extend(std::iter::repeat_n(0u8, 3 * w));
    let name = n.name.as_bytes();
    let name = name.get(..name.len().min(255)).unwrap_or(&[]);
    out.push(name.len() as u8);
    out.extend_from_slice(name);
    let values_start = out.len();
    for v in &n.values {
        value(out, v, compress);
    }
    let values_len = out.len() - values_start;
    if !n.children.is_empty() {
        for c in &n.children {
            record(out, c, wide, compress);
        }
        null(out, wide);
    }
    let end = out.len();
    number(out, start, end, wide);
    number(out, start + w, n.values.len(), wide);
    number(out, start + 2 * w, values_len, wide);
}

fn value(out: &mut Vec<u8>, v: &Value, compress: bool) {
    let bytes = |out: &mut Vec<u8>, code: u8, b: &[u8]| {
        out.push(code);
        out.extend((b.len() as u32).to_le_bytes());
        out.extend_from_slice(b);
    };
    match v {
        Value::Bool(x) => out.extend([b'C', *x]),
        Value::Byte(x) => out.extend([b'B', *x]),
        Value::I16(x) => {
            out.push(b'Y');
            out.extend(x.to_le_bytes());
        }
        Value::I32(x) => {
            out.push(b'I');
            out.extend(x.to_le_bytes());
        }
        Value::I64(x) => {
            out.push(b'L');
            out.extend(x.to_le_bytes());
        }
        Value::F32(x) => {
            out.push(b'F');
            out.extend(x.to_le_bytes());
        }
        Value::F64(x) => {
            out.push(b'D');
            out.extend(x.to_le_bytes());
        }
        Value::String(s) => bytes(out, b'S', s.as_bytes()),
        Value::Raw(b) => bytes(out, b'R', b),
        Value::Array(a) => {
            let (code, count, raw): (u8, usize, Vec<u8>) = match a {
                Array::Bool(v) => (b'b', v.len(), v.clone()),
                Array::Byte(v) => (b'c', v.len(), v.clone()),
                Array::I32(v) => (b'i', v.len(), v.iter().flat_map(|x| x.to_le_bytes()).collect()),
                Array::I64(v) => (b'l', v.len(), v.iter().flat_map(|x| x.to_le_bytes()).collect()),
                Array::F32(v) => (b'f', v.len(), v.iter().flat_map(|x| x.to_le_bytes()).collect()),
                Array::F64(v) => (b'd', v.len(), v.iter().flat_map(|x| x.to_le_bytes()).collect()),
            };
            let (encoding, data) =
                if compress { (1u32, miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6)) } else { (0, raw) };
            out.push(code);
            out.extend((count as u32).to_le_bytes());
            out.extend(encoding.to_le_bytes());
            out.extend((data.len() as u32).to_le_bytes());
            out.extend(data);
        }
    }
}

/// `P: name, type, "", flags, values...`.
pub fn prop(name: &str, type_name: &str, values: Vec<Value>) -> Node {
    let mut all = vec![
        Value::String(name.into()),
        Value::String(type_name.into()),
        Value::String(String::new()),
        Value::String("A".into()),
    ];
    all.extend(values);
    Node::new("P", all, Vec::new())
}

/// A property of three numbers (`Lcl Translation`, `Color`, ...).
pub fn prop3(name: &str, type_name: &str, v: [f64; 3]) -> Node {
    prop(name, type_name, v.into_iter().map(Value::F64).collect())
}

/// An object record: `class: id, "name\0\x01class", sub_class { Properties70 { props }, rest }`.
pub fn object(class: &str, id: i64, name: &str, sub_class: &str, props: Vec<Node>, rest: Vec<Node>) -> Node {
    let mut children = vec![Node::new("Properties70", Vec::new(), props)];
    children.extend(rest);
    Node::new(
        class,
        vec![
            Value::I64(id),
            Value::String(format!("{name}\u{0}\u{1}{class}")),
            Value::String(sub_class.into()),
        ],
        children,
    )
}

/// A `C` connection record, `kind` `OO` or `OP` (with the parent's property).
pub fn connection(kind: &str, child: i64, parent: i64, property: Option<&str>) -> Node {
    let mut v = vec![Value::String(kind.into()), Value::I64(child), Value::I64(parent)];
    if let Some(p) = property {
        v.push(Value::String(p.into()));
    }
    Node::new("C", v, Vec::new())
}

/// A record holding one value.
pub fn leaf(name: &str, v: Value) -> Node {
    Node::new(name, vec![v], Vec::new())
}

/// A cube of side 1 around the origin as a `Geometry` record: six quads, normals per corner,
/// texture coordinates through an index, one material for all.
pub fn cube_geometry(id: i64, name: &str) -> Node {
    let p: Vec<f64> =
        (0..8).flat_map(|i| [i & 1, (i >> 1) & 1, (i >> 2) & 1].map(|b| f64::from(b) - 0.5)).collect();
    // Corners counter-clockwise seen from outside, last one stored as -i - 1.
    let quads: [([i32; 4], [f64; 3]); 6] = [
        ([0, 2, 3, 1], [0.0, 0.0, -1.0]),
        ([4, 5, 7, 6], [0.0, 0.0, 1.0]),
        ([0, 1, 5, 4], [0.0, -1.0, 0.0]),
        ([2, 6, 7, 3], [0.0, 1.0, 0.0]),
        ([0, 4, 6, 2], [-1.0, 0.0, 0.0]),
        ([1, 3, 7, 5], [1.0, 0.0, 0.0]),
    ];
    let index: Vec<i32> = quads.iter().flat_map(|(q, _)| [q[0], q[1], q[2], -q[3] - 1]).collect();
    let normals: Vec<f64> = quads.iter().flat_map(|(_, n)| [*n; 4].into_iter().flatten()).collect();
    let layer = |name: &str, children: Vec<Node>| Node::new(name, vec![Value::I32(0)], children);
    let text = |name: &str, v: &str| leaf(name, Value::String(v.into()));
    let mut g = object("Geometry", id, name, "Mesh", Vec::new(), Vec::new());
    g.children.extend([
        leaf("Vertices", Value::Array(Array::F64(p))),
        leaf("PolygonVertexIndex", Value::Array(Array::I32(index))),
        layer(
            "LayerElementNormal",
            vec![
                text("MappingInformationType", "ByPolygonVertex"),
                text("ReferenceInformationType", "Direct"),
                leaf("Normals", Value::Array(Array::F64(normals))),
            ],
        ),
        layer(
            "LayerElementUV",
            vec![
                text("Name", "map1"),
                text("MappingInformationType", "ByPolygonVertex"),
                text("ReferenceInformationType", "IndexToDirect"),
                leaf("UV", Value::Array(Array::F64(vec![0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0]))),
                leaf("UVIndex", Value::Array(Array::I32((0..24).map(|i| i % 4).collect()))),
            ],
        ),
        layer(
            "LayerElementMaterial",
            vec![
                text("MappingInformationType", "AllSame"),
                text("ReferenceInformationType", "IndexToDirect"),
                leaf("Materials", Value::Array(Array::I32(vec![0]))),
            ],
        ),
    ]);
    g
}

/// A small scene for tests and examples, y up, in centimetres: a red cube `base` of 1 m
/// (scaled by 100), lifted 50 cm and turned 45 degrees about y, and a cube `top` as its child,
/// one unit (1 m) above it, half its size, with a pre-rotation of 90 degrees about z and a
/// geometric offset. The `Model` template supplies `RotationActive`.
pub fn example_scene() -> Vec<Node> {
    let i = |name: &str, v: i64| prop(name, "int", vec![Value::I32(v as i32)]);
    let settings = Node::new(
        "GlobalSettings",
        Vec::new(),
        vec![Node::new(
            "Properties70",
            Vec::new(),
            vec![
                i("UpAxis", 1),
                i("UpAxisSign", 1),
                i("FrontAxis", 2),
                i("FrontAxisSign", 1),
                i("CoordAxis", 0),
                i("CoordAxisSign", 1),
                prop("UnitScaleFactor", "double", vec![Value::F64(1.0)]),
            ],
        )],
    );
    let template = Node::new(
        "ObjectType",
        vec![Value::String("Model".into())],
        vec![Node::new(
            "PropertyTemplate",
            vec![Value::String("FbxNode".into())],
            vec![Node::new(
                "Properties70",
                Vec::new(),
                vec![i("RotationActive", 1), prop3("Lcl Scaling", "Lcl Scaling", [1.0; 3])],
            )],
        )],
    );
    let definitions = Node::new("Definitions", Vec::new(), vec![template]);
    let base = object(
        "Model",
        10,
        "base",
        "Mesh",
        vec![
            prop3("Lcl Translation", "Lcl Translation", [0.0, 50.0, 0.0]),
            prop3("Lcl Rotation", "Lcl Rotation", [0.0, 45.0, 0.0]),
            prop3("Lcl Scaling", "Lcl Scaling", [100.0; 3]),
        ],
        Vec::new(),
    );
    let top = object(
        "Model",
        11,
        "top",
        "Mesh",
        vec![
            prop3("Lcl Translation", "Lcl Translation", [0.0, 1.0, 0.0]),
            prop3("PreRotation", "Vector3D", [0.0, 0.0, 90.0]),
            prop3("Lcl Scaling", "Lcl Scaling", [0.5; 3]),
            prop3("GeometricTranslation", "Vector3D", [0.0, 0.5, 0.0]),
        ],
        Vec::new(),
    );
    let red = object(
        "Material",
        30,
        "red",
        "",
        vec![
            prop3("DiffuseColor", "Color", [0.8, 0.1, 0.1]),
            prop("DiffuseFactor", "double", vec![Value::F64(1.0)]),
        ],
        Vec::new(),
    );
    let objects = Node::new(
        "Objects",
        Vec::new(),
        vec![base, top, cube_geometry(20, "base_mesh"), cube_geometry(21, "top_mesh"), red],
    );
    let connections = Node::new(
        "Connections",
        Vec::new(),
        vec![
            connection("OO", 10, 0, None),
            connection("OO", 11, 10, None),
            connection("OO", 20, 10, None),
            connection("OO", 21, 11, None),
            connection("OO", 30, 10, None),
            connection("OO", 30, 11, None),
        ],
    );
    vec![
        Node::new("FBXHeaderExtension", Vec::new(), vec![leaf("FBXVersion", Value::I32(7400))]),
        settings,
        definitions,
        objects,
        connections,
    ]
}

/// Key interpolation flags for [`curve`]: constant, linear, cubic with auto tangents.
pub const KEY_CONSTANT: i32 = 0x2;
pub const KEY_LINEAR: i32 = 0x4;
pub const KEY_CUBIC: i32 = 0x8;

/// An `AnimationCurve` record with keys at `seconds` holding `values`, all interpolated by
/// `flags` (one attribute group for every key, slopes zero).
pub fn curve(id: i64, seconds: &[f64], values: &[f32], flags: i32) -> Node {
    let ticks: Vec<i64> =
        seconds.iter().map(|s| (s * crate::anim::TICKS_PER_SECOND as f64).round() as i64).collect();
    object(
        "AnimationCurve",
        id,
        "",
        "",
        Vec::new(),
        vec![
            leaf("Default", Value::F64(0.0)),
            leaf("KeyVer", Value::I32(4009)),
            leaf("KeyTime", Value::Array(Array::I64(ticks))),
            leaf("KeyValueFloat", Value::Array(Array::F32(values.to_vec()))),
            leaf("KeyAttrFlags", Value::Array(Array::I32(vec![flags]))),
            leaf("KeyAttrDataFloat", Value::Array(Array::F32(vec![0.0; 4]))),
            leaf("KeyAttrRefCount", Value::Array(Array::I32(vec![seconds.len() as i32]))),
        ],
    )
}

/// [`example_scene`] with a take of 2 seconds at 30 frames per second: `base` rises from 50 to
/// 150 cm (linear, its y), and `top` turns from 0 to 90 degrees about its z (cubic).
pub fn animated_scene() -> Vec<Node> {
    let mut nodes = example_scene();
    let ticks = |s: f64| Value::I64((s * crate::anim::TICKS_PER_SECOND as f64).round() as i64);
    let stack = object(
        "AnimationStack",
        40,
        "Take 001",
        "",
        vec![prop("LocalStart", "KTime", vec![ticks(0.0)]), prop("LocalStop", "KTime", vec![ticks(2.0)])],
        Vec::new(),
    );
    let layer = object("AnimationLayer", 41, "BaseLayer", "", Vec::new(), Vec::new());
    let node = |id: i64, name: &str, v: [f64; 3]| {
        object(
            "AnimationCurveNode",
            id,
            name,
            "",
            vec![
                prop("d|X", "Number", vec![Value::F64(v[0])]),
                prop("d|Y", "Number", vec![Value::F64(v[1])]),
                prop("d|Z", "Number", vec![Value::F64(v[2])]),
            ],
            Vec::new(),
        )
    };
    let added = vec![
        stack,
        layer,
        node(42, "T", [0.0, 50.0, 0.0]),
        node(43, "R", [0.0, 0.0, 0.0]),
        curve(44, &[0.0, 2.0], &[50.0, 150.0], KEY_LINEAR),
        curve(45, &[0.0, 2.0], &[0.0, 90.0], KEY_CUBIC),
    ];
    let links = vec![
        connection("OO", 41, 40, None),
        connection("OO", 42, 41, None),
        connection("OO", 43, 41, None),
        connection("OP", 42, 10, Some("Lcl Translation")),
        connection("OP", 43, 11, Some("Lcl Rotation")),
        connection("OP", 44, 42, Some("d|Y")),
        connection("OP", 45, 43, Some("d|Z")),
    ];
    for n in &mut nodes {
        match n.name.as_str() {
            "Objects" => n.children.extend(added.clone()),
            "Connections" => n.children.extend(links.clone()),
            "GlobalSettings" => {
                if let Some(p) = n.children.first_mut() {
                    p.children.push(prop("TimeMode", "enum", vec![Value::I32(6)]));
                }
            }
            _ => {}
        }
    }
    nodes
}

/// 16 numbers, column after column, as FBX stores a matrix: a translation by `t`.
fn translation_array(t: [f64; 3]) -> Value {
    let [x, y, z] = t;
    let m = vec![1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, x, y, z, 1.0];
    Value::Array(Array::F64(m))
}

/// A skinned column for tests and examples, y up, in centimetres, with a take of 2 seconds at
/// 30 frames per second.
///
/// The model `body` places [`cube_geometry`] scaled to 1 x 2 x 1 m (by its geometric scaling),
/// centred at the origin. Two bones carry it: `hip` at y -100 cm and its child `knee` at the
/// origin. The bottom four control points follow `hip`, the top four `knee`, except point 7
/// (top, x and z positive), which has weights 0.5 for `hip` and 1.5 for `knee` (a quarter and
/// three quarters once divided by their sum). The `knee` cluster stores `Transform` and
/// `TransformLink`; the `hip` cluster stores neither, so its bind matrix comes from the
/// `BindPose`. In the take, `knee` turns from 0 to 90 degrees about z (linear).
pub fn skinned_scene() -> Vec<Node> {
    let mut nodes = example_scene();
    let ticks = |s: f64| Value::I64((s * crate::anim::TICKS_PER_SECOND as f64).round() as i64);
    let model =
        |id: i64, name: &str, sub: &str, props: Vec<Node>| object("Model", id, name, sub, props, Vec::new());
    let body = model(10, "body", "Mesh", vec![prop3("GeometricScaling", "Vector3D", [100.0, 200.0, 100.0])]);
    let hip =
        model(12, "hip", "LimbNode", vec![prop3("Lcl Translation", "Lcl Translation", [0.0, -100.0, 0.0])]);
    let knee =
        model(13, "knee", "LimbNode", vec![prop3("Lcl Translation", "Lcl Translation", [0.0, 100.0, 0.0])]);
    let mut skin = object("Deformer", 50, "skin", "Skin", Vec::new(), Vec::new());
    skin.children.push(leaf("SkinningType", Value::String("Linear".into())));
    let cluster = |id: i64,
                   name: &str,
                   indexes: Vec<i32>,
                   weights: Vec<f64>,
                   matrices: Option<([f64; 3], [f64; 3])>| {
        let mut c = object("Deformer", id, name, "Cluster", Vec::new(), Vec::new());
        c.children.push(leaf("Indexes", Value::Array(Array::I32(indexes))));
        c.children.push(leaf("Weights", Value::Array(Array::F64(weights))));
        if let Some((transform, link)) = matrices {
            c.children.push(leaf("Transform", translation_array(transform)));
            c.children.push(leaf("TransformLink", translation_array(link)));
        }
        c
    };
    let hip_cluster = cluster(51, "hip", vec![0, 1, 4, 5, 7], vec![1.0, 1.0, 1.0, 1.0, 0.5], None);
    let knee_cluster =
        cluster(52, "knee", vec![2, 3, 6, 7], vec![1.0, 1.0, 1.0, 1.5], Some(([0.0; 3], [0.0; 3])));
    let mut pose = object("Pose", 60, "BindPose", "BindPose", Vec::new(), Vec::new());
    pose.children.push(leaf("Type", Value::String("BindPose".into())));
    pose.children.push(Node::new(
        "PoseNode",
        Vec::new(),
        vec![leaf("Node", Value::I64(12)), leaf("Matrix", translation_array([0.0, -100.0, 0.0]))],
    ));
    let stack = object(
        "AnimationStack",
        40,
        "Take 001",
        "",
        vec![prop("LocalStart", "KTime", vec![ticks(0.0)]), prop("LocalStop", "KTime", vec![ticks(2.0)])],
        Vec::new(),
    );
    let layer = object("AnimationLayer", 41, "BaseLayer", "", Vec::new(), Vec::new());
    let turn = object(
        "AnimationCurveNode",
        43,
        "R",
        "",
        vec![
            prop("d|X", "Number", vec![Value::F64(0.0)]),
            prop("d|Y", "Number", vec![Value::F64(0.0)]),
            prop("d|Z", "Number", vec![Value::F64(0.0)]),
        ],
        Vec::new(),
    );
    let objects = vec![
        body,
        hip,
        knee,
        cube_geometry(20, "body_mesh"),
        skin,
        hip_cluster,
        knee_cluster,
        pose,
        stack,
        layer,
        turn,
        curve(45, &[0.0, 2.0], &[0.0, 90.0], KEY_LINEAR),
    ];
    let links = vec![
        connection("OO", 10, 0, None),
        connection("OO", 12, 0, None),
        connection("OO", 13, 12, None),
        connection("OO", 20, 10, None),
        connection("OO", 30, 10, None),
        connection("OO", 50, 20, None),
        connection("OO", 51, 50, None),
        connection("OO", 52, 50, None),
        connection("OO", 12, 51, None),
        connection("OO", 13, 52, None),
        connection("OO", 41, 40, None),
        connection("OO", 43, 41, None),
        connection("OP", 43, 13, Some("Lcl Rotation")),
        connection("OP", 45, 43, Some("d|Z")),
    ];
    for n in &mut nodes {
        match n.name.as_str() {
            "Objects" => {
                // Keep the material of the example scene, replace its models and geometry.
                n.children.retain(|c| c.name == "Material");
                n.children.extend(objects.clone());
            }
            "Connections" => n.children = links.clone(),
            "GlobalSettings" => {
                if let Some(p) = n.children.first_mut() {
                    p.children.push(prop("TimeMode", "enum", vec![Value::I32(6)]));
                }
            }
            _ => {}
        }
    }
    nodes
}
