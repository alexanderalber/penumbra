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

    // Run again on its own output with the same rings: the first output is still held, so the
    // ring hands out another buffer and the program never writes what it reads.
    let again = run(&gpu, &program, &out, &mut outputs, 0.5);
    assert_eq!(read4(&gpu, &again, "position")[2], [2.0, 4.0, 8.0, 1.0]);
    let buffer = |p: &Points| p.attr("position").unwrap().buffer().clone();
    assert!(!buffer(&again).same(&buffer(&out)));
    // Fed back for a few more cooks, dropping what is no longer read: the ring reuses a buffer
    // nobody holds, never the input's.
    let mut last = again;
    for k in 0..4 {
        let next = run(&gpu, &program, &last, &mut outputs, 0.5);
        assert!(!buffer(&next).same(&buffer(&last)), "cook {k}");
        assert_eq!(read4(&gpu, &next, "position")[2][2], 9.0 + k as f32, "cook {k}");
        last = next;
    }
    drop(out);
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

#[test]
fn names_wgsl_or_the_program_already_has_are_refused() {
    // Builtin functions and types, the prelude's globals, and names WGSL keeps for itself.
    for (name, needle) in [
        ("max", "WGSL function"),
        ("distance", "WGSL function"),
        ("vec3", "WGSL"),
        ("f32", "WGSL"),
        ("params", "every program has"),
        ("pic0", "every program has"),
        ("vol_out", "every program has"),
        ("index", "every program has"),
        ("_", "cannot name"),
        ("__x", "cannot name"),
        ("pn_x", "penumbra's own"),
        ("filter", "reserved"),
    ] {
        let e = check_name(name).err().unwrap_or_else(|| panic!("`{name}` was accepted"));
        assert!(e.0.contains(needle) && e.0.contains("choose another name"), "{name}: {e}");
    }
    for name in ["heat", "max_speed", "a", "x2"] {
        assert!(check_name(name).is_ok(), "{name}");
    }
    for s in super::super::STANDARD {
        assert!(check_name(s.name).is_ok(), "{}", s.name);
    }
    // Every path that takes a name: a declaration in code, a header line.
    let shape = Shape::new(&[("position", AttrType::Vec3)]);
    let max = spec("fn main() {}", vec![Decl::set("max").ty(AttrType::F32)]);
    assert!(generate(&max, &shape, 8).err().is_some_and(|e| e.0.contains("WGSL function")));
    assert!(parse_header("// @write distance float\nfn main() {}").is_err());
}

#[test]
fn a_program_keeps_buffers_only_for_what_it_writes() {
    let Some(gpu) = gpu() else { return };
    let input = points(&gpu, 1000, 1000);
    let mut outputs = Outputs::default();
    // A name typed letter by letter: each one a program of its own over the same rings.
    for name in ["a", "ab", "abc"] {
        let decls = vec![Decl::set(name).ty(AttrType::F32)];
        let source = format!("fn main() {{ {name} = 1.0; }}");
        let g = generate(&spec(&source, decls), &Shape::of(&input), storage_budget(&gpu)).unwrap();
        let out = run(&gpu, &Program::compile(&gpu, g).unwrap(), &input, &mut outputs, 1.0);
        assert!(out.attr(name).is_some());
    }
    assert_eq!(outputs.bytes(), 1000 * 4, "one attribute's worth");
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

#[test]
fn no_points_still_carry_what_the_program_writes() {
    let Some(gpu) = gpu() else { return };
    let source = "fn main() { heat = 2.0; }";
    for (capacity, count) in [(4, 0), (0, 0)] {
        let input = if capacity == 0 { Points::empty() } else { points(&gpu, count, capacity) };
        let decls = vec![Decl::set("heat").ty(AttrType::F32)];
        let g = generate(&spec(source, decls), &Shape::of(&input), storage_budget(&gpu)).unwrap();
        let out = run(&gpu, &Program::compile(&gpu, g).unwrap(), &input, &mut Outputs::default(), 1.0);
        assert_eq!(out.count().known(), Some(0));
        let heat = out.attr("heat").unwrap_or_else(|| panic!("capacity {capacity}: no heat"));
        assert_eq!(heat.ty(), AttrType::F32);
        assert!(heat.len() >= u64::from(capacity));
    }
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

#[test]
fn whole_number_defaults_reach_the_ends_of_their_range() {
    let Some(gpu) = gpu() else { return };
    let input = points(&gpu, 2, 2);
    let decls = vec![
        Decl::read("low").ty(AttrType::I32).default([-2_147_483_648.0; 4]),
        Decl::read("high").ty(AttrType::U32).default([4_294_967_296.0; 4]),
        Decl::set("low_out").ty(AttrType::I32),
        Decl::set("high_out").ty(AttrType::U32),
    ];
    let source = "fn main() { low_out = low; high_out = high; }";
    let g = generate(&spec(source, decls), &Shape::of(&input), storage_budget(&gpu)).unwrap();
    let program = Program::compile(&gpu, g).unwrap_or_else(|e| panic!("{e}"));
    let out = run(&gpu, &program, &input, &mut Outputs::default(), 1.0);
    assert_eq!(out.attr("low_out").unwrap().buffer().read::<i32>(&gpu).unwrap()[0], i32::MIN);
    assert_eq!(out.attr("high_out").unwrap().buffer().read::<u32>(&gpu).unwrap()[0], u32::MAX);
}

#[test]
fn a_header_with_endless_declarations_is_refused_at_once() {
    let mut source: String = (0..20_000).map(|i| format!("// @read a{i} float\n")).collect();
    source.push_str("fn main() {}\n");
    let started = std::time::Instant::now();
    let e = parse_header(&source).err().map(|e| e.0).unwrap_or_default();
    assert!(e.contains("declarations"), "{e}");
    assert!(started.elapsed().as_secs_f64() < 1.0, "{:?}", started.elapsed());
    // The same from code: more declarations than any program can use.
    let decls: Vec<Decl> = (0..20_000).map(|i| Decl::read(&format!("a{i}")).ty(AttrType::F32)).collect();
    let started = std::time::Instant::now();
    let e =
        generate(&spec("fn main() {}", decls), &Shape::default(), 8).err().map(|e| e.0).unwrap_or_default();
    assert!(e.contains("declarations"), "{e}");
    assert!(started.elapsed().as_secs_f64() < 1.0, "{:?}", started.elapsed());
}

#[test]
fn a_written_vector_has_its_fourth_number_set_and_helpers_count_as_uses() {
    let Some(gpu) = gpu() else { return };
    let input = points(&gpu, 3, 3);
    let source = "
fn pts_speed() -> f32 {
    return time * 0.0 + 2.0;
}

fn main() {
    velocity = vec3f(pts_speed());
}";
    let g = generate(&spec(source, vec![Decl::set("velocity")]), &Shape::of(&input), storage_budget(&gpu))
        .unwrap();
    let program = Program::compile(&gpu, g).unwrap();
    assert!(program.uses_time(), "time read through a function of the source");
    let out = run(&gpu, &program, &input, &mut Outputs::default(), 1.0);
    assert_eq!(read4(&gpu, &out, "velocity")[2], [2.0, 2.0, 2.0, 0.0]);
}

#[test]
fn a_header_asks_for_neighbours_by_metres_or_by_a_param() {
    let near = |s: &str| parse_header(s).map(|h| h.near);
    assert_eq!(near("fn main() {}").unwrap(), None);
    assert_eq!(
        near("// @neighbors 0.25\nfn main() {}").unwrap(),
        Some(NearDecl { radius: Radius::Metres(0.25), cap: DEFAULT_NEAR_CAP })
    );
    assert_eq!(
        near("// @param reach float = 0.1\n// @neighbors reach 200 -- how far\nfn main() {}").unwrap(),
        Some(NearDecl { radius: Radius::Param("reach".into()), cap: 200 })
    );
    for bad in [
        "// @neighbors",
        "// @neighbors -1",
        "// @neighbors far",
        "// @neighbors 0.1 0",
        "// @neighbors 0.1 9999",
    ] {
        let e = near(bad).unwrap_err().0;
        assert!(e.contains("line 1"), "{bad}: {e}");
    }
    assert!(near("// @neighbors 0.1\n// @neighbors 0.2").unwrap_err().0.contains("line 2"));
    assert!(near("// @neighbours 0.1").unwrap_err().0.contains("@neighbors"), "a misspelling is named");
    // Its names are a program's own: no attribute takes them.
    assert!(check_name("near_begin").is_err() && check_name("Near").is_err());
    // Two more storage buffers, counted against the budget.
    let s = spec("fn main() {}", vec![Decl::write("position")]);
    let shape = Shape::new(&[("position", AttrType::Vec3)]);
    assert_eq!(generate_near(&s, &shape, 8, true).unwrap().bindings(), 4);
    assert!(generate_near(&s, &shape, 3, true).is_err());
}
