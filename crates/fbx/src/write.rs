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
