//! Programs over points on WARP: attributes created, passed on, read as their default, read and
//! written at once; the header of a file; the binding budget; counts beyond one row.

use super::*;
use crate::testing::gpu;
use penumbra_core::Value;

/// `n` points with a position (i, 2i, 3i) and a scale i, in buffers of `capacity`.
fn points(gpu: &Gpu, n: u32, capacity: u32) -> Points {
    let pos: Vec<[f32; 4]> = (0..capacity).map(|i| [i as f32, 2.0 * i as f32, 3.0 * i as f32, 1.0]).collect();
    let scale: Vec<f32> = (0..capacity).map(|i| i as f32).collect();
    let p = Buffer::of::<[f32; 4]>(gpu, u64::from(capacity), "p").unwrap();
    p.write(gpu, 0, &pos).unwrap();
    let s = Buffer::of::<f32>(gpu, u64::from(capacity), "s").unwrap();
    s.write(gpu, 0, &scale).unwrap();
    let attrs = vec![
        Attr::new("position", AttrType::Vec3, p).unwrap(),
        Attr::new("scale", AttrType::F32, s).unwrap(),
    ];
    Points::new(capacity, n, attrs).unwrap().with_bounds(penumbra_mesh::Bounds::of([[0.0; 3], [1.0; 3]]))
}

fn spec(source: &str, decls: Vec<Decl>) -> Spec {
    Spec { label: "test".into(), source: source.into(), decls, params: vec![ParamSpec::float("gain", 1.0)] }
}

fn run(gpu: &Gpu, program: &Program, input: &Points, outputs: &mut Outputs, gain: f64) -> Points {
    let param = move |n: &str| (n == "gain").then_some(Value::Float(gain));
    let r = Run { seconds: 2.0, frame: 120, dt: 0.5, param: &param, pictures: &[] };
    program.run(gpu, input, outputs, &r).unwrap()
}

fn read4(gpu: &Gpu, p: &Points, name: &str) -> Vec<[f32; 4]> {
    p.attr(name).unwrap().buffer().read::<[f32; 4]>(gpu).unwrap()
}

#[test]
fn programs_create_pass_on_default_and_update_attributes() {
    let Some(gpu) = gpu() else { return };
    let input = points(&gpu, 5, 8);
    let source = "
fn main() {
    position.z += params.gain * f32(index);
    color = vec4f(index01, mass, weight, 0.5);
    heat = scale * 2.0 + dt;
    tag = count;
}";
    let decls = vec![
        Decl::write("P"),
        Decl::set("Color"),
        Decl::read("mass"),
        Decl::read("weight").default([3.0; 4]),
        Decl::read("scale"),
        Decl::set("heat").ty(AttrType::F32),
        Decl::set("tag").ty(AttrType::U32),
    ];
    let generated = generate(&spec(source, decls), &Shape::of(&input), storage_budget(&gpu)).unwrap();
    // Read: position and scale (mass and weight are missing); written: position, color, heat, tag.
    assert_eq!(generated.bindings(), 6, "{}", generated.source());
    assert_eq!(generated.writes().collect::<Vec<_>>(), ["position", "color", "heat", "tag"]);
    let program = Program::compile(&gpu, generated).unwrap();
    assert!(program.uses_time() && !program.uses_picture(0));
    let mut outputs = Outputs::default();
    let out = run(&gpu, &program, &input, &mut outputs, 0.5);
    assert_eq!(out.count(), input.count());
    assert_eq!(out.capacity(), 8);
    assert!(out.bounds().is_none(), "moved positions drop the bounds");

    // The input is untouched, `scale` passes on as the same buffer.
    assert_eq!(read4(&gpu, &input, "position")[2], [2.0, 4.0, 6.0, 1.0]);
    assert!(out.attr("scale").unwrap().buffer().same(input.attr("scale").unwrap().buffer()));
    assert!(!out.attr("position").unwrap().buffer().same(input.attr("position").unwrap().buffer()));

    let pos = read4(&gpu, &out, "position");
    assert_eq!(pos[2], [2.0, 4.0, 7.0, 1.0], "read and written: 6 + 0.5 * 2, fourth float 1");
    let color = read4(&gpu, &out, "color");
    // Mass reads as its standard default 1, weight as the declared 3.
    assert_eq!(color[0], [0.0, 1.0, 3.0, 0.5]);
    assert_eq!(color[4], [1.0, 1.0, 3.0, 0.5]);
    let heat = out.attr("heat").unwrap().buffer().read::<f32>(&gpu).unwrap();
    assert_eq!(heat[..5], [0.5, 2.5, 4.5, 6.5, 8.5]);
    let tag = out.attr("tag").unwrap().buffer().read::<u32>(&gpu).unwrap();
    assert_eq!(tag[..5], [5; 5]);
    assert_eq!(out.attr("tag").unwrap().ty(), AttrType::U32);

    // Run again: new buffers from the ring while the first output is still held.
    let again = run(&gpu, &program, &out, &mut Outputs::default(), 0.5);
    assert_eq!(read4(&gpu, &again, "position")[2], [2.0, 4.0, 8.0, 1.0]);
    assert!(
        Program::compile(&gpu, generate(&spec(source, vec![]), &Shape::of(&input), 16).unwrap()).is_err()
    );
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

#[test]
fn a_program_says_what_it_cannot_do() {
    let Some(gpu) = gpu() else { return };
    let input = points(&gpu, 4, 4);
    let shape = Shape::of(&input);
    let err = |source: &str, decls: Vec<Decl>, budget: u32| {
        generate(&spec(source, decls), &shape, budget).err().map(|e| e.0).unwrap_or_default()
    };
    let main = "fn main() {}";
    assert!(err("fn other() {}", vec![], 8).contains("fn main()"));
    assert!(err(main, vec![Decl::set("heat")], 8).contains("declare it"));
    assert!(
        err(main, vec![Decl::read("scale").ty(AttrType::Vec3)], 8)
            .contains("is f32, the program reads it as vec3f")
    );
    assert!(err(main, vec![Decl::read("index")], 8).contains("a name every program has"));
    assert!(err(main, vec![Decl::read("filter")], 8).contains("reserved"));
    assert!(err(main, vec![Decl::read("pn_x").ty(AttrType::F32)], 8).contains("pn_"));
    assert!(err(main, vec![Decl::read("a b")], 8).contains("cannot name"));
    let conflicting = vec![Decl::read("heat").ty(AttrType::F32), Decl::write("heat").ty(AttrType::U32)];
    assert!(err(main, conflicting, 8).contains("declared as f32 and as u32"));
    // Three attributes read and written: color is missing from the input and reads as its
    // default, so they take five buffers, not six. More than a budget of four.
    let three = vec![Decl::write("position"), Decl::write("scale"), Decl::write("color")];
    let e = err(main, three.clone(), 4);
    assert!(e.contains("5 storage buffers") && e.contains("gives a program 4"), "{e}");
    assert_eq!(generate(&spec(main, three), &shape, 5).unwrap().bindings(), 5);

    // More than the device binds: an error from the generator, never a panic in wgpu.
    let budget = storage_budget(&gpu);
    let many: Vec<Decl> = (0..=budget).map(|i| Decl::set(&format!("a{i}")).ty(AttrType::F32)).collect();
    let e = err(main, many, budget);
    assert!(e.contains("this GPU gives a program"), "{e}");

    // A program made for one set of attributes refuses another.
    let p = Program::compile(&gpu, generate(&spec(main, vec![Decl::read("scale")]), &shape, budget).unwrap())
        .unwrap();
    let other = input.without("scale");
    assert!(!p.generated().fits(&other));
    let param = |_: &str| None;
    let r = Run { seconds: 0.0, frame: 0, dt: 0.0, param: &param, pictures: &[] };
    assert!(p.run(&gpu, &other, &mut Outputs::default(), &r).is_err());
    // A compile error names the line of the source.
    let broken =
        generate(&spec("\nfn main() { position = 1; }", vec![Decl::write("position")]), &shape, budget);
    let e = Program::compile(&gpu, broken.unwrap()).err().unwrap();
    assert!(e.0.contains("test:2"), "{e}");
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

#[test]
fn counts_beyond_one_row_reach_every_point() {
    let Some(gpu) = gpu() else { return };
    // Three rows of the dispatch grid, the last one short.
    let n = 2 * ROW + 5;
    let s = Buffer::of::<f32>(&gpu, u64::from(n), "s").unwrap();
    let p = Points::new(n, n, vec![Attr::new("scale", AttrType::F32, s).unwrap()]).unwrap();
    let g = generate(&spec("fn main() { scale = f32(index); }", vec![Decl::set("scale")]), &Shape::of(&p), 7)
        .unwrap();
    let program = Program::compile(&gpu, g).unwrap();
    assert!(!program.uses_time());
    let out = run(&gpu, &program, &p, &mut Outputs::default(), 1.0);
    let v = out.attr("scale").unwrap().buffer().read::<f32>(&gpu).unwrap();
    assert_eq!(v.len(), n as usize);
    for i in [0, 1, ROW - 1, ROW, 2 * ROW, n - 1] {
        assert_eq!(v[i as usize], i as f32, "{i}");
    }
    assert_eq!(grid(n), (ROW, 3));
    assert_eq!(grid(5), (5, 1));
}

#[test]
fn headers_declare_attributes_and_name_mistakes() {
    let h = parse_header(
        "// @param height float = 0.1\n\
         // @read N\n\
         // @write P\n\
         // @write heat float = 2 -- how hot\n\
         // @write tint vec4 = [1, 0.5, 0, 1]\n\
         // @todo something\n\
         fn main() {}",
    )
    .unwrap();
    assert_eq!(h.params.len(), 1);
    assert_eq!(h.decls[0], Decl::read("normal"));
    assert_eq!(h.decls[1], Decl::write("position"));
    assert_eq!(h.decls[2], Decl::write("heat").ty(AttrType::F32).default([2.0; 4]));
    assert_eq!(h.decls[3].default, Some([1.0, 0.5, 0.0, 1.0]));
    for (src, needle) in [
        ("// @reed position", "misspelt `@read`"),
        ("// @wirte position", "misspelt `@write`"),
        ("\n// @write heat quaternion", "line 2"),
        ("// @write heat float = abc", "no number"),
        ("// @write heat float = [1, 2, 3, 4, 5]", "up to 4"),
        ("// @read time", "a name every program has"),
        ("// @read", "expected"),
        ("// @param pn_count int", "pn_"),
        ("// @parm x float", "misspelt `@param`"),
    ] {
        let e = parse_header(src).err().unwrap_or_else(|| panic!("{src} parsed"));
        assert!(e.0.contains(needle), "{src}: {e}");
    }
}
