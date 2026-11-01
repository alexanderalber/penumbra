use std::collections::HashMap;

use super::*;
use crate::write::{combine, to_glb, to_obj, to_stl};

fn no_files(r: &str) -> Result<Vec<u8>, Error> {
    Err(Error::new(format!("no file `{r}` in this test")))
}

fn files(map: HashMap<&'static str, Vec<u8>>) -> impl Fn(&str) -> Result<Vec<u8>, Error> {
    move |r| map.get(r).cloned().ok_or_else(|| Error::new(format!("no `{r}`")))
}

fn named(mut m: Mesh, name: &str) -> Mesh {
    m.parts = vec![name.to_string()];
    m
}

/// A box, a sphere beside it and a plane behind: three parts.
fn scene() -> Mesh {
    let mut red = prim::cuboid([1.0, 1.0, 1.0]).unwrap();
    red.materials = vec![Material::plain("red", [0.8, 0.1, 0.1, 1.0])];
    combine(&[
        named(red, "body"),
        named(prim::moved(prim::sphere(0.5, 24).unwrap(), [1.5, 0.0, 0.0]), "head"),
        named(prim::moved(prim::plane([4.0, 2.0]).unwrap(), [0.0, 0.0, -1.0]), "wall"),
    ])
}

fn close(a: [f32; 3], b: [f32; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4)
}

#[test]
fn primitives_are_consistent_and_face_outwards() {
    for m in [
        prim::cuboid([2.0, 1.0, 0.5]).unwrap(),
        prim::sphere(1.0, 16).unwrap(),
        prim::plane([1.0, 1.0]).unwrap(),
        prim::grid([1.0, 2.0], 3).unwrap(),
    ] {
        m.check().unwrap();
        let c = m.bounds().unwrap().center();
        for t in m.indices.as_chunks::<3>().0 {
            let p = [0, 1, 2].map(|k| m.vertices[t[k] as usize].position);
            let n = face_normal(p);
            let mid = [0, 1, 2].map(|i| (p[0][i] + p[1][i] + p[2][i]) / 3.0 - c[i]);
            // Every face's winding normal points away from the centre (the plane: towards +z).
            let d = n[0] * mid[0] + n[1] * mid[1] + n[2] * mid[2];
            assert!(d > 0.0 || (m.parts[0] == "plane" && n[1] < -0.99), "{:?} {n:?} {mid:?}", m.parts);
            // The stored normal agrees with the winding.
            let vn = m.vertices[t[0] as usize].normal;
            assert!(vn[0] * n[0] + vn[1] * n[1] + vn[2] * n[2] > 0.0);
        }
    }
    assert_eq!(prim::cuboid([1.0; 3]).unwrap().triangle_count(), 12);
    let b = prim::cuboid([2.0, 1.0, 0.5]).unwrap().bounds().unwrap();
    assert!(close(b.min, [-1.0, -0.5, -0.25]) && close(b.max, [1.0, 0.5, 0.25]));
}

#[test]
fn a_grid_keeps_its_cell_edges_and_hides_the_diagonals() {
    let m = prim::grid([4.0, 2.0], 4).unwrap();
    m.check().unwrap();
    assert_eq!((m.vertices.len(), m.triangle_count()), (25, 32));
    let b = m.bounds().unwrap();
    assert!(close(b.min, [-2.0, 0.0, -1.0]) && close(b.max, [2.0, 0.0, 1.0]));
    // The edges not marked as inside a quad are exactly the grid's lines: 5 rows and 5 columns
    // of 4 segments each, every one of them axis-aligned.
    let mut lines = std::collections::HashSet::new();
    for (t, mask) in m.indices.as_chunks::<3>().0.iter().zip(&m.inner_edges) {
        for k in 0..3 {
            let (a, b) = (t[(k + 1) % 3], t[(k + 2) % 3]);
            let (pa, pb) = (m.vertices[a as usize].position, m.vertices[b as usize].position);
            if mask & (1 << k) == 0 {
                assert!(pa[0] == pb[0] || pa[2] == pb[2], "{pa:?} {pb:?}");
                lines.insert((a.min(b), a.max(b)));
            } else {
                assert!(pa[0] != pb[0] && pa[2] != pb[2], "a cell edge marked inside");
            }
        }
    }
    assert_eq!(lines.len(), 2 * 5 * 4);
    // One cell is the plane as before.
    let one = prim::grid([4.0, 2.0], 1).unwrap();
    assert_eq!(one.triangle_count(), prim::plane([4.0, 2.0]).unwrap().triangle_count());
}

#[test]
fn y_up_turns_into_z_up() {
    // A y-up triangle facing +z (glTF's front) faces -y afterwards, its top at +z.
    let mut m = prim::plane([2.0, 2.0]).unwrap();
    for v in &mut m.vertices {
        // Undo the plane's z up into a y-up wall facing +z: (x, y, z) -> (x, z, -y).
        let [x, y, z] = v.position;
        v.position = [x, z, -y];
        let [a, b, c] = v.normal;
        v.normal = [a, c, -b];
    }
    m.to_z_up(Up::Y);
    let back = prim::plane([2.0, 2.0]).unwrap();
    assert_eq!(m.vertices, back.vertices);
    assert_eq!(Up::of_file("x.STL"), Up::Z);
    assert_eq!(Up::of_file("x.glb"), Up::Y);
    assert_eq!(Up::of_file("x.obj"), Up::Y);
}

#[test]
fn obj_round_trip_keeps_parts_and_numbers() {
    let s = scene();
    let text = to_obj(&s, None);
    let mut back = parse("scene.obj", text.as_bytes(), &no_files, &Limits::default()).unwrap();
    back.mesh.to_z_up(Up::Y);
    let m = &back.mesh;
    assert_eq!(m.parts, ["body", "head", "wall"]);
    assert_eq!(m.triangle_count(), s.triangle_count());
    let info = m.part_info();
    assert_eq!(info.iter().map(|p| p.triangles).collect::<Vec<_>>(), [12, s.part_info()[1].triangles, 2]);
    assert!(close(info[1].bounds.unwrap().center(), [1.5, 0.0, 0.0]));
    assert!(close(m.bounds().unwrap().min, s.bounds().unwrap().min));
    // Texture coordinates come back in the top left convention.
    let uv = |m: &Mesh, i: &u32| m.vertices[*i as usize].uv;
    assert!(m.indices.iter().zip(&s.indices).all(|(a, b)| (uv(m, a)[1] - uv(&s, b)[1]).abs() < 1e-5));
}

#[test]
fn obj_details() {
    let mtl =
        b"newmtl wood\nKd 1 0.5 0\nmap_Kd -s 1 1 1 tex/wood.png\nnewmtl glass\nKd 0 0 1\nd 0.25\n".to_vec();
    let obj = "mtllib parts.mtl
v 0 0 0
v 1 0 0
v 1 1 0
v 0 1 0
vt 0 0
vt 1 1
g left
usemtl wood
f 1/1 2/2 3 4
g right
usemtl glass
f -4 -2 -1
usemtl nothing
f 1 2 3
g left
f 1 3 4
";
    let side = files(HashMap::from([("parts.mtl", mtl), ("tex/wood.png", b"PNG".to_vec())]));
    let l = parse("x.obj", obj.as_bytes(), &side, &Limits::default()).unwrap();
    let m = &l.mesh;
    assert_eq!(m.parts, ["left", "right"]);
    // A quad (two triangles), one, one with an unknown material, one more for `left`.
    assert_eq!(m.triangle_count(), 5);
    assert_eq!(m.part_info().iter().map(|p| p.triangles).collect::<Vec<_>>(), [3, 2]);
    let wood = &m.materials[m.ranges[0].material as usize];
    assert_eq!(wood.name, "wood");
    assert!((wood.color[1] - srgb_to_linear(0.5)).abs() < 1e-6);
    assert_eq!(&*m.images[wood.texture.unwrap() as usize].bytes, b"PNG");
    let glass = m.materials.iter().find(|x| x.name == "glass").unwrap();
    assert_eq!(glass.color[3], 0.25);
    // No normals in the file: flat ones facing +z.
    assert!(m.vertices.iter().all(|v| close(v.normal, [0.0, 0.0, 1.0])));
    // `vt 0 0` is the bottom left in OBJ, so v = 1 here.
    assert_eq!(m.vertices[0].uv, [0.0, 1.0]);
    assert!(l.warnings.iter().any(|w| w.contains("usemtl")), "{:?}", l.warnings);
    // A missing library is a warning, not an error.
    let l = parse("x.obj", obj.as_bytes(), &no_files, &Limits::default()).unwrap();
    assert!(l.warnings.iter().any(|w| w.contains("material library")));
    // Bad input is an error that says where.
    for bad in [
        "v 0 0\nf 1 2 3\n",
        "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 4\n",
        "v nan 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n",
        "f 0 1 2\n",
    ] {
        assert!(parse("x.obj", bad.as_bytes(), &no_files, &Limits::default()).is_err(), "{bad}");
    }
    assert!(parse("x.obj", b"# nothing\n", &no_files, &Limits::default()).is_err());
}

#[test]
fn stl_binary_and_ascii() {
    let s = scene();
    let bin = to_stl(&s);
    let l = parse("thing.stl", &bin, &no_files, &Limits::default()).unwrap();
    assert_eq!(l.mesh.parts, ["thing"]);
    assert_eq!(l.mesh.triangle_count(), s.triangle_count());
    assert!(close(l.mesh.bounds().unwrap().min, s.bounds().unwrap().min));
    let ascii = "solid t\nfacet normal 0 0 0\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\nendloop\nendfacet\n\
                 facet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 2 0 0\nendloop\nendfacet\nendsolid t\n";
    let l = parse("t.stl", ascii.as_bytes(), &no_files, &Limits::default()).unwrap();
    assert_eq!(l.mesh.triangle_count(), 1);
    assert!(close(l.mesh.vertices[0].normal, [0.0, 0.0, 1.0]));
    assert!(l.warnings.is_empty());
    // A count that does not match the size is not binary, and not ASCII either.
    let mut wrong = bin.clone();
    wrong[80..84].copy_from_slice(&1_000_000u32.to_le_bytes());
    assert!(parse("t.stl", &wrong, &no_files, &Limits::default()).is_err());
}

#[test]
fn glb_round_trip_and_gltf_with_data_uri() {
    let s = scene();
    let glb = to_glb(&s);
    let mut l = parse("scene.glb", &glb, &no_files, &Limits::default()).unwrap();
    l.mesh.to_z_up(Up::Y);
    assert!(close(l.mesh.bounds().unwrap().min, s.bounds().unwrap().min));
    assert!(close(l.mesh.bounds().unwrap().max, s.bounds().unwrap().max));
    assert_eq!(l.mesh.parts, ["body", "head", "wall"]);
    assert_eq!(l.mesh.triangle_count(), s.triangle_count());
    assert!(close(l.mesh.part_info()[1].bounds.unwrap().center(), [1.5, 0.0, 0.0]));
    assert_eq!(l.mesh.materials.iter().find(|m| m.name == "red").unwrap().color, [0.8, 0.1, 0.1, 1.0]);

    // One triangle in a data URI, under a parent node that moves it by 2 in x and mirrors z,
    // in a child that scales by 2.
    let mut buf = Vec::new();
    for c in [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
        buf.extend(c.to_le_bytes());
    }
    let b64 = encode_b64(&buf);
    let json = format!(
        r#"{{"asset":{{"version":"2.0"}},
        "buffers":[{{"byteLength":36,"uri":"data:application/octet-stream;base64,{b64}"}}],
        "bufferViews":[{{"buffer":0,"byteLength":36}}],
        "accessors":[{{"bufferView":0,"count":3,"type":"VEC3","componentType":5126}}],
        "meshes":[{{"name":"tri","primitives":[{{"attributes":{{"POSITION":0}}}}]}}],
        "nodes":[{{"translation":[2,0,0],"scale":[1,1,-1],"children":[1]}},{{"mesh":0,"scale":[2,2,2]}}]}}"#
    );
    let l = parse("t.gltf", json.as_bytes(), &no_files, &Limits::default()).unwrap();
    let m = &l.mesh;
    assert_eq!(m.parts, ["tri"]);
    let p: Vec<[f32; 3]> = m.indices.iter().map(|i| m.vertices[*i as usize].position).collect();
    // Mirrored, so the winding is flipped to keep facing the same way as the normal.
    assert!(close(p[0], [2.0, 0.0, 0.0]));
    assert!(p.contains(&[4.0, 0.0, 0.0]) && p.contains(&[2.0, 2.0, 0.0]));
    // Facing +z before the mirror, -z after.
    assert!(close(m.vertices[0].normal, [0.0, 0.0, -1.0]));
    assert!(close(face_normal([p[0], p[1], p[2]]), [0.0, 0.0, -1.0]));
}

fn encode_b64(data: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for k in 0..4 {
            if k <= c.len() {
                s.push(A[((n >> (18 - 6 * k)) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

#[test]
fn base64_decodes() {
    for data in [&b""[..], b"a", b"ab", b"abc", b"abcd", &[0, 255, 7, 128, 3]] {
        assert_eq!(gltf::base64_for_tests(&encode_b64(data)).unwrap(), data);
    }
    assert!(gltf::base64_for_tests("ab$c").is_err());
}

#[test]
fn budgets_are_checked_before_allocating() {
    // A 300-byte glTF claiming a billion vertices (the spike's 12 GB file): refused by count.
    let json = r#"{"asset":{"version":"2.0"},"buffers":[{"byteLength":12,"uri":"data:;base64,AAAAAAAAAAAAAAAA"}],
        "bufferViews":[{"buffer":0,"byteLength":12}],
        "accessors":[{"bufferView":0,"count":1000000000,"type":"VEC3","componentType":5126}],
        "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],"nodes":[{"mesh":0}]}"#;
    let e = parse("x.gltf", json.as_bytes(), &no_files, &Limits::default()).unwrap_err();
    assert!(e.0.contains("more than"), "{e}");
    // Offsets near the end of the address space: an error, not an overflow.
    let json = json
        .replace("1000000000", "1")
        .replace(r#""buffer":0,"#, r#""buffer":0,"byteOffset":18446744073709551615,"#);
    assert!(parse("x.gltf", json.as_bytes(), &no_files, &Limits::default()).is_err());
    // Shared children make a walk exponential: capped.
    let mut nodes = Vec::new();
    for i in 0..40 {
        nodes.push(format!(r#"{{"children":[{0},{0},{0}]}}"#, i + 1));
    }
    nodes.push("{}".into());
    let json = format!(
        r#"{{"asset":{{"version":"2.0"}},"scenes":[{{"nodes":[0]}}],"nodes":[{}]}}"#,
        nodes.join(",")
    );
    let e = parse("x.gltf", json.as_bytes(), &no_files, &Limits::default()).unwrap_err();
    assert!(e.0.contains("node visits") || e.0.contains("deeper"), "{e}");
    // A cycle ends at the depth limit.
    let json =
        r#"{"asset":{"version":"2.0"},"nodes":[{"children":[1]},{"children":[0]}],"scenes":[{"nodes":[0]}]}"#;
    assert!(parse("x.gltf", json.as_bytes(), &no_files, &Limits::default()).is_err());
    // Too many triangles for the budget.
    let small = Limits { max_triangles: 10, ..Limits::default() };
    let e = parse("s.obj", to_obj(&scene(), None).as_bytes(), &no_files, &small).unwrap_err();
    assert!(e.0.contains("triangles"), "{e}");
    let e = parse("s.stl", &to_stl(&scene()), &no_files, &small).unwrap_err();
    assert!(e.0.contains("triangles"), "{e}");
    // A file over the size limit.
    let tiny = Limits { max_file_bytes: 100, ..Limits::default() };
    assert!(parse("s.stl", &to_stl(&scene()), &no_files, &tiny).is_err());
}

#[test]
fn sidecars_stay_relative() {
    let dir = Path::new("models");
    for bad in
        ["/etc/passwd", "\\\\server\\share\\x.png", "C:\\x.png", "c:x.png", "http://x/y.png", "file:///x", ""]
    {
        assert!(sidecar_path(dir, bad).is_err(), "{bad}");
    }
    assert_eq!(sidecar_path(dir, "tex/a%20b.png").unwrap(), dir.join("tex/a b.png"));
    assert_eq!(sidecar_path(dir, "../tex/a.png").unwrap(), dir.join("../tex/a.png"));
}

#[test]
fn files_on_disk() {
    let dir = penumbra_core::scratch::ScratchDir::new("mesh").unwrap();
    std::fs::create_dir_all(dir.join("tex")).unwrap();
    std::fs::write(dir.join("m.mtl"), "newmtl a\nKd 1 1 1\nmap_Kd tex/a.png\n").unwrap();
    std::fs::write(dir.join("tex/a.png"), b"not really").unwrap();
    std::fs::write(dir.join("m.obj"), "mtllib m.mtl\nusemtl a\nv 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n")
        .unwrap();
    let l = load_file(&dir.join("m.obj"), &Limits::default()).unwrap();
    assert_eq!(l.mesh.images.len(), 1);
    let e = load_file(&dir.join("missing.obj"), &Limits::default()).unwrap_err();
    assert!(e.0.contains("missing.obj"));
}

#[test]
fn smooth_normals_round_off_faceted_surfaces() {
    let flat = |m: &Mesh| parse("m.stl", &to_stl(m), &no_files, &Limits::default()).unwrap().mesh;
    // A faceted sphere comes out with its normals along the radius.
    let mut ball = flat(&prim::sphere(1.0, 24).unwrap());
    let worst = |m: &Mesh| {
        m.vertices
            .iter()
            .map(|v| {
                let p = normalize(v.position);
                p[0] * v.normal[0] + p[1] * v.normal[1] + p[2] * v.normal[2]
            })
            .fold(1.0f32, f32::min)
    };
    assert!(worst(&ball) < 0.99, "faceted to begin with");
    ball.smooth_normals(180.0, 1, usize::MAX).unwrap();
    ball.check().unwrap();
    assert!(worst(&ball) > 0.99, "{}", worst(&ball));
    // A box: all smooth, its corners point diagonally; with a crease below 90 degrees its faces
    // stay flat.
    let cube = flat(&prim::cuboid([2.0; 3]).unwrap());
    let mut round = cube.clone();
    round.smooth_normals(180.0, 1, usize::MAX).unwrap();
    let d = 1.0 / 3f32.sqrt();
    assert!(round.vertices.iter().all(|v| close(v.normal, v.position.map(|c| c.signum() * d))));
    let mut hard = cube.clone();
    hard.smooth_normals(30.0, 1, usize::MAX).unwrap();
    assert_eq!(hard.vertices, cube.vertices);
    // A box of eight shared corners: the crease splits each corner in three, one per face.
    let welded = || {
        let limits = Limits::default();
        let mut b = Builder::new(&limits);
        let part = b.part("box").unwrap();
        let mut v = Vec::new();
        for k in 0..8u32 {
            let p = [k & 1, (k >> 1) & 1, (k >> 2) & 1].map(|bit| if bit == 1 { 1.0 } else { -1.0 });
            v.push(b.vertex(Vertex { position: p, normal: [0.0; 3], uv: [0.0; 2] }).unwrap());
        }
        // Counter-clockwise from outside.
        for [a, b2, c, d] in
            [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]]
        {
            b.triangle([v[a], v[b2], v[c]], part, 0).unwrap();
            b.triangle([v[a], v[c], v[d]], part, 0).unwrap();
        }
        b.finish()
    };
    let mut shared = welded();
    shared.smooth_normals(30.0, 1, usize::MAX).unwrap();
    shared.check().unwrap();
    assert_eq!(shared.vertices.len(), 24);
    assert!(shared.vertices.iter().all(|v| v.normal.iter().filter(|c| c.abs() > 0.99).count() == 1));
    let mut all = welded();
    all.smooth_normals(180.0, 1, usize::MAX).unwrap();
    assert_eq!(all.vertices.len(), 8, "nothing to split");
    assert!(all.vertices.iter().all(|v| close(v.normal, v.position.map(|c| c * d))));
    // Over the vertex budget: an error, the mesh as it was.
    let mut capped = welded();
    assert!(capped.smooth_normals(30.0, 1, 12).is_err());
    assert_eq!(capped.vertices, welded().vertices);
}

#[test]
fn smoothing_many_faces_on_one_point_stays_linear() {
    // 200 000 triangles fanned around one point: no quadratic work, finite normals.
    let limits = Limits::default();
    let mut b = Builder::new(&limits);
    let part = b.part("fan").unwrap();
    for k in 0..200_000u32 {
        let a = k as f32 * 0.001;
        b.flat_triangle(
            [[0.0; 3], [a.cos(), a.sin(), 0.1], [(a + 0.01).cos(), (a + 0.01).sin(), 0.0]],
            [[0.0; 2]; 3],
            part,
            0,
        )
        .unwrap();
    }
    let mut m = b.finish();
    let t = std::time::Instant::now();
    m.smooth_normals(60.0, 1, usize::MAX).unwrap();
    assert!(t.elapsed() < std::time::Duration::from_secs(20), "{:?}", t.elapsed());
    assert!(m.vertices.iter().all(|v| v.normal.iter().all(|c| c.is_finite())));
}

#[test]
fn more_rings_round_off_a_staircase() {
    // A slope of one in three built from unit steps, as a voxel tool writes it: runs of three
    // flat cells, then a riser; extruded along y.
    let limits = Limits::default();
    let mut b = Builder::new(&limits);
    let part = b.part("stairs").unwrap();
    let mut profile = vec![[0.0f32, 0.0]];
    for step in 0..12 {
        let z = step as f32;
        for k in 1..=3 {
            profile.push([(step * 3 + k) as f32, z]);
        }
        profile.push([(step * 3 + 3) as f32, z + 1.0]);
    }
    for w in profile.windows(2) {
        let ([x0, z0], [x1, z1]) = (w[0], w[1]);
        for y in 0..4 {
            let (y0, y1) = (y as f32, y as f32 + 1.0);
            let q = [[x0, y0, z0], [x1, y0, z1], [x1, y1, z1], [x0, y1, z0]];
            b.flat_triangle([q[0], q[1], q[2]], [[0.0; 2]; 3], part, 0).unwrap();
            b.flat_triangle([q[0], q[2], q[3]], [[0.0; 2]; 3], part, 0).unwrap();
        }
    }
    let stairs = b.finish();
    // The slope's normal, and how far the normals in the middle stray from it at worst.
    let slope = normalize([-1.0, 0.0, 3.0]);
    let spread = |rings: u32| {
        let mut m = stairs.clone();
        m.smooth_normals(180.0, rings, usize::MAX).unwrap();
        m.vertices
            .iter()
            .filter(|v| (6.0..30.0).contains(&v.position[0]))
            .map(|v| {
                (v.normal[0] * slope[0] + v.normal[1] * slope[1] + v.normal[2] * slope[2]).acos().to_degrees()
            })
            .fold(0.0f32, f32::max)
    };
    let (one, six) = (spread(1), spread(6));
    assert!(one > 15.0, "one ring keeps the steps: {one}");
    assert!(six < one * 0.5, "six rings: {six}, one: {one}");
}

#[test]
fn a_matrix_is_baked_into_positions_and_normals() {
    let cube = prim::cuboid([2.0, 2.0, 2.0]).unwrap();
    // Scale x by 2, turn 90 degrees about z (x to y), move up by 5.
    let m = [[0.0, -1.0, 0.0, 0.0], [2.0, 0.0, 0.0, 0.0], [0.0, 0.0, 1.0, 5.0], [0.0, 0.0, 0.0, 1.0]];
    let moved = cube.transformed(&m);
    moved.check().unwrap();
    assert_eq!((moved.indices.clone(), moved.parts.clone()), (cube.indices.clone(), cube.parts.clone()));
    for (a, b) in cube.vertices.iter().zip(&moved.vertices) {
        let [x, y, z] = a.position;
        assert!(close(b.position, [-y, 2.0 * x, z + 5.0]), "{a:?} -> {b:?}");
        // Normals turn with the box and stay unit length; the scale does not tilt an axis normal.
        let [nx, ny, nz] = a.normal;
        assert!(close(b.normal, [-ny, nx, nz]), "{a:?} -> {b:?}");
    }
    // A slanted normal leans towards the squashed axis, as the inverse transpose says.
    let mut slant = cube.clone();
    slant.vertices[0].normal = normalize([1.0, 1.0, 0.0]);
    let squash = [[0.5, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
    assert!(close(slant.transformed(&squash).vertices[0].normal, normalize([2.0, 1.0, 0.0])));
    // A mirror reverses the corners, so every face still points outwards.
    let mirror = [[-1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
    let mirrored = cube.transformed(&mirror);
    for t in mirrored.indices.as_chunks::<3>().0 {
        let p = t.map(|i| mirrored.vertices[i as usize].position);
        let n = face_normal(p);
        let c = [0, 1, 2].map(|k| (p[0][k] + p[1][k] + p[2][k]) / 3.0);
        assert!(n[0] * c[0] + n[1] * c[1] + n[2] * c[2] > 0.0, "a face turned inwards: {p:?}");
        assert!(close(mirrored.vertices[t[0] as usize].normal, n), "the normal follows the face");
    }
}

#[test]
fn append_renumbers_and_keeps_polygon_edges() {
    let grid = prim::grid([1.0, 1.0], 2).unwrap();
    let cube = prim::cuboid([1.0; 3]).unwrap();
    let mut out = cube.clone();
    out.append(&grid);
    out.check().unwrap();
    assert_eq!(out.triangle_count(), cube.triangle_count() + grid.triangle_count());
    assert_eq!(out.parts.len(), cube.parts.len() + grid.parts.len());
    assert_eq!(out.inner_edges.len(), out.triangle_count(), "padded for the cube's triangles");
    assert_eq!(out.inner_edges[cube.triangle_count()..], grid.inner_edges[..]);
}

/// The example FBX scene of `penumbra-fbx`, optionally with a diffuse texture on its material:
/// embedded (`content`) or beside the file as `tex/wood.png`.
fn fbx_scene(texture: Option<&[u8]>) -> Vec<u8> {
    penumbra_fbx::write::to_bytes(7400, &fbx_nodes(texture), true)
}

/// The records of [`fbx_scene`].
fn fbx_nodes(texture: Option<&[u8]>) -> Vec<penumbra_fbx::Node> {
    use penumbra_fbx::Value;
    use penumbra_fbx::write::{connection, leaf, object};
    let mut nodes = penumbra_fbx::write::example_scene();
    if let Some(content) = texture {
        let objects = nodes.iter_mut().find(|n| n.name == "Objects").unwrap();
        objects.children.push(object(
            "Texture",
            40,
            "wood",
            "",
            Vec::new(),
            vec![
                leaf("FileName", Value::String("C:/Users/someone/tex/wood.png".into())),
                leaf("RelativeFilename", Value::String(r"tex\wood.png".into())),
            ],
        ));
        objects.children.push(object(
            "Video",
            50,
            "wood",
            "Clip",
            Vec::new(),
            vec![leaf("Content", Value::Raw(content.to_vec()))],
        ));
        let conns = nodes.iter_mut().find(|n| n.name == "Connections").unwrap();
        conns.children.push(connection("OP", 40, 30, Some("DiffuseColor")));
        conns.children.push(connection("OO", 50, 40, None));
    }
    nodes
}

#[test]
fn fbx_loads_as_parts_in_metres_and_z_up() {
    let bytes = fbx_scene(None);
    let l = parse("scene.fbx", &bytes, &no_files, &Limits::default()).unwrap();
    // Sniffed by its magic, whatever the name.
    assert_eq!(parse("scene.bin", &bytes, &no_files, &Limits::default()).unwrap(), l);
    assert_eq!(Up::of_file("x.FBX"), Up::Z);
    let m = &l.mesh;
    assert_eq!(m.parts, ["base", "top"]);
    assert_eq!(m.triangle_count(), 24);
    assert!(l.warnings.is_empty(), "{:?}", l.warnings);
    let info = m.part_info();
    // The base cube: 1 m, standing on the floor (z = 0), turned 45 degrees: its corners reach
    // out to half a diagonal.
    let b = info[0].bounds.unwrap();
    let h = std::f32::consts::FRAC_1_SQRT_2;
    assert!(close(b.min, [-h, -h, 0.0]) && close(b.max, [h, h, 1.0]), "{b:?}");
    // The top cube sits 1.5 m up, half the size; y up became z up, z front became -y.
    let c = info[1].bounds.unwrap().center();
    let o = 0.25 * h;
    assert!(close(c, [-o, -o, 1.5]), "{c:?}");
    // Quads split along an inner edge, and normals pointing outwards.
    assert_eq!(m.inner_edges.iter().filter(|e| **e != 0).count(), 24);
    for t in m.indices.as_chunks::<3>().0 {
        let p = t.map(|i| m.vertices[i as usize].position);
        let n = m.vertices[t[0] as usize].normal;
        assert!(close(face_normal(p), n), "{p:?} {n:?}");
    }
    assert_eq!(m.materials[1].name, "red");
    assert!(m.ranges.iter().all(|r| r.material == 1));
}

#[test]
fn fbx_textures_come_embedded_or_from_beside_the_file() {
    let embedded =
        parse("s.fbx", &fbx_scene(Some(b"\x89PNG embedded")), &no_files, &Limits::default()).unwrap();
    assert_eq!(&*embedded.mesh.images[0].bytes, b"\x89PNG embedded");
    assert_eq!(embedded.mesh.materials[1].texture, Some(0));
    // No content: the relative path, with its backslash turned.
    let side = files(HashMap::from([("tex/wood.png", b"beside".to_vec())]));
    let beside = parse("s.fbx", &fbx_scene(Some(b"")), &side, &Limits::default()).unwrap();
    assert_eq!(&*beside.mesh.images[0].bytes, b"beside");
    // Neither: a warning, the colour stays.
    let none = parse("s.fbx", &fbx_scene(Some(b"")), &no_files, &Limits::default()).unwrap();
    assert!(
        none.mesh.images.is_empty() && none.warnings.iter().any(|w| w.contains("wood")),
        "{:?}",
        none.warnings
    );
}

#[test]
fn ascii_fbx_files_load_as_their_binary_twins() {
    use penumbra_fbx::write::{animated_scene, skinned_scene, to_ascii, to_bytes};
    let l = Limits::default();
    for nodes in [fbx_nodes(Some(b"\x89PNG embedded")), animated_scene(), skinned_scene()] {
        let binary = parse("s.fbx", &to_bytes(7400, &nodes, false), &no_files, &l).unwrap();
        let text = to_ascii(7400, &nodes);
        let ascii = parse("s.fbx", text.as_bytes(), &no_files, &l).unwrap();
        assert_eq!(ascii, binary);
        // Sniffed by its header comment, whatever the name.
        assert_eq!(parse("s.txt", text.as_bytes(), &no_files, &l).unwrap(), binary);
    }
}

#[test]
fn fbx_errors_say_why() {
    let old = b"; FBX 6.1.0 project file\nFBXHeaderExtension:  {\n}\n";
    let e = parse("a.fbx", old, &no_files, &Limits::default()).unwrap_err();
    assert!(e.0.contains("FBX 6"), "{e}");
    let open = b"; FBX 7.4.0 project file\nObjects: {\n";
    let e = parse("a.fbx", open, &no_files, &Limits::default()).unwrap_err();
    assert!(e.0.contains("not closed"), "{e}");
    let cut = fbx_scene(None);
    assert!(parse("a.fbx", &cut[..cut.len() / 2], &no_files, &Limits::default()).is_err());
}

#[test]
fn animated_fbx_files_come_with_their_motion() {
    let bytes = penumbra_fbx::write::to_bytes(7400, &penumbra_fbx::write::animated_scene(), false);
    let none = |_: &str| Err(Error::new("no sidecar"));
    let l = parse("a.fbx", &bytes, &none, &Limits::default()).unwrap();
    let m = l.motion.expect("motion");
    assert_eq!(m.piece.len(), l.mesh.vertices.len());
    assert_eq!(m.takes.len(), 1);
    // A second in, the base (piece 0) has risen 50 cm along z; at 0 nothing moved.
    let mut out = Vec::new();
    m.deform(&l.mesh.vertices, 0, 0.0, &mut out);
    assert!(
        out.iter()
            .zip(&l.mesh.vertices)
            .all(|(a, b)| (0..3).all(|k| (a.position[k] - b.position[k]).abs() < 1e-5))
    );
    m.deform(&l.mesh.vertices, 0, 1.0, &mut out);
    for (i, (a, b)) in out.iter().zip(&l.mesh.vertices).enumerate() {
        if m.piece[i] == 0 {
            assert!((a.position[2] - b.position[2] - 0.5).abs() < 1e-5);
        }
    }
    // Control points come along: the cube has 8.
    assert!(m.point.iter().all(|p| *p < 8));
}

#[test]
fn skinned_fbx_files_bend_with_their_bones() {
    let bytes = penumbra_fbx::write::to_bytes(7400, &penumbra_fbx::write::skinned_scene(), false);
    let none = |_: &str| Err(Error::new("no sidecar"));
    let mut l = parse("s.fbx", &bytes, &none, &Limits::default()).unwrap();
    assert!(l.warnings.is_empty(), "{:?}", l.warnings);
    // Only the bones move, yet the file moves.
    let mut m = l.motion.take().expect("motion");
    assert!(m.moves(0));
    // Turned to y up and with smoothed normals, as geo.file may do: the motion follows.
    l.mesh.to_z_up(Up::Y);
    m.to_z_up(Up::Y);
    let split = l.mesh.smooth_normals(30.0, 1, 1 << 20).unwrap();
    m.split(&split);
    let mut out = Vec::new();
    m.deform(&l.mesh.vertices, 0, 0.0, &mut out);
    for (a, b) in out.iter().zip(&l.mesh.vertices) {
        assert!((0..3).all(|k| (a.position[k] - b.position[k]).abs() < 1e-5), "{a:?} {b:?}");
        assert!((0..3).all(|k| (a.normal[k] - b.normal[k]).abs() < 1e-4), "{a:?} {b:?}");
    }
    // Two seconds in, the knee has turned 90 degrees. Read into penumbra's axes (metres, z up,
    // front -y) and then turned once more, file point (x, y, z) cm lands at (x, -y, -z) / 100.
    m.deform(&l.mesh.vertices, 0, 2.0, &mut out);
    for (i, v) in out.iter().enumerate() {
        let want: [f32; 3] = match m.point[i] {
            0 => [-0.5, 1.0, 0.5],
            2 => [-1.0, 0.5, 0.5],
            7 => [-0.625, -0.625, -0.5],
            _ => continue,
        };
        assert!(
            (0..3).all(|k| (v.position[k] - want[k]).abs() < 1e-5),
            "point {}: {:?}",
            m.point[i],
            v.position
        );
        assert!((v.normal.iter().map(|c| c * c).sum::<f32>() - 1.0).abs() < 1e-4);
    }
}
