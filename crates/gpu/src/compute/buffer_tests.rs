//! Storage buffers in compute programs: creation, writes, resizing, readback, dispatches over
//! ranges and grids, per-step uniforms, atomics, and the errors for what cannot work.

use penumbra_core::{ParamSpec, Value};

use crate::buffer::unique_buffer_bytes;
use crate::testing::gpu;
use crate::{Buffer, BufferSlot, Compute, Dispatch, Out, Step, Target, Tex};

fn none(_: &str) -> Option<Value> {
    None
}

fn plain(param: &dyn Fn(&str) -> Option<Value>) -> Dispatch<'_> {
    Dispatch { volumes: &[], pictures: &[], data: None, seconds: 0.0, frame: 0, param }
}

#[test]
fn a_buffer_starts_at_zero_and_takes_writes_in_range() {
    let Some(gpu) = gpu() else { return };
    let b = Buffer::of::<u32>(&gpu, 10, "b").unwrap();
    assert_eq!(b.bytes(), 40);
    assert_eq!(b.len_of::<u32>(), 10);
    assert_eq!(b.read::<u32>(&gpu).unwrap(), [0; 10]);
    b.write(&gpu, 3, &[7u32, 8, 9]).unwrap();
    assert_eq!(b.read::<u32>(&gpu).unwrap(), [0, 0, 0, 7, 8, 9, 0, 0, 0, 0]);
    // Elements of another type count in their own size.
    b.write(&gpu, 4, &[[1.5f32, 2.5]]).unwrap();
    let floats: Vec<f32> = b.read(&gpu).unwrap();
    assert_eq!(&floats[8..10], [1.5, 2.5]);
    assert_eq!(b.read::<u32>(&gpu).unwrap()[3..6], [7, 8, 9]);
    // Past the end, and off whole words.
    let e = b.write(&gpu, 8, &[1u32, 2, 3]).err().unwrap();
    assert!(e.0.contains("do not fit"), "{e}");
    assert!(b.write(&gpu, u64::MAX, &[1u32]).is_err());
    let e = b.write(&gpu, 0, &[1u8, 2, 3]).err().unwrap();
    assert!(e.0.contains("multiple of 4"), "{e}");
    assert!(b.write(&gpu, 1, &[5u16]).is_err());
    b.write::<u32>(&gpu, 0, &[]).unwrap();
    // Reading as elements that do not divide the size.
    assert!(b.read::<[u8; 3]>(&gpu).is_err());
    b.clear(&gpu).unwrap();
    assert_eq!(b.read::<u32>(&gpu).unwrap(), [0; 10]);
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

#[test]
fn sizes_beyond_the_devices_limits_are_errors() {
    let Some(gpu) = gpu() else { return };
    let max = Buffer::max_bytes(&gpu);
    let limits = gpu.device().limits();
    assert_eq!(max, limits.max_storage_buffer_binding_size.min(limits.max_buffer_size));
    for bytes in [0, 6] {
        let e = Buffer::new(&gpu, bytes, "odd").err().unwrap();
        assert!(e.0.contains("multiple of 4"), "{e}");
    }
    let e = Buffer::new(&gpu, max + 4, "huge").err().unwrap();
    assert!(e.0.contains("`huge`") && e.0.contains(&max.to_string()), "{e}");
    assert!(Buffer::of::<[f32; 4]>(&gpu, u64::MAX, "overflow").is_err());
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

#[test]
fn a_resized_buffer_keeps_what_fits() {
    let Some(gpu) = gpu() else { return };
    let mut b = Buffer::of::<u32>(&gpu, 4, "b").unwrap();
    b.write(&gpu, 0, &[1u32, 2, 3, 4]).unwrap();
    let old = b.clone();
    b.resize(&gpu, 32, "b").unwrap();
    assert_eq!(b.bytes(), 32);
    assert_eq!(b.read::<u32>(&gpu).unwrap(), [1, 2, 3, 4, 0, 0, 0, 0]);
    assert!(!b.same(&old));
    assert_eq!(old.read::<u32>(&gpu).unwrap(), [1, 2, 3, 4]);
    assert_eq!(unique_buffer_bytes([&b, &old, &b]), 48);
    b.resize(&gpu, 8, "b").unwrap();
    assert_eq!(b.read::<u32>(&gpu).unwrap(), [1, 2]);
    // A size that cannot be leaves the buffer as it was.
    assert!(b.resize(&gpu, 7, "b").is_err());
    assert_eq!(b.read::<u32>(&gpu).unwrap(), [1, 2]);
}

#[test]
fn a_readback_without_waiting_delivers_after_a_poll_and_reuses_its_buffer() {
    let Some(gpu) = gpu() else { return };
    let b = Buffer::of::<u32>(&gpu, 4, "b").unwrap();
    b.write(&gpu, 0, &[1u32, 2, 3, 4]).unwrap();
    let first = b.read_async(&gpu, None).unwrap();
    assert!(first.get::<u32>().is_err() || first.state() == crate::ReadState::Ready);
    assert_eq!(first.wait(&gpu, std::time::Duration::from_secs(30)), crate::ReadState::Ready);
    assert_eq!(first.get::<u32>().unwrap(), [1, 2, 3, 4]);
    assert!(first.get::<[u8; 3]>().is_err());
    // A later write shows in the next readback, made in the first one's staging buffer.
    b.write(&gpu, 1, &[9u32]).unwrap();
    let second = b.read_async(&gpu, Some(first)).unwrap();
    assert_eq!(second.wait(&gpu, std::time::Duration::from_secs(30)), crate::ReadState::Ready);
    assert_eq!(second.get::<u32>().unwrap(), [1, 9, 3, 4]);
    // A spare of another size is not taken.
    let small = Buffer::of::<u32>(&gpu, 2, "small").unwrap();
    let third = small.read_async(&gpu, Some(second)).unwrap();
    assert_eq!(third.wait(&gpu, std::time::Duration::from_secs(30)), crate::ReadState::Ready);
    assert_eq!(third.get::<u32>().unwrap(), [0, 0]);
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

const RANGES: &str = "
@group(2) @binding(0) var<storage, read_write> out: array<u32>;
@group(2) @binding(1) var<storage, read_write> seen: array<u32>;

@compute @workgroup_size(64)
fn line(@builtin(global_invocation_id) id: vec3u) {
    if (id.x == 0u) {
        seen[0] = pn.size.x;
    }
    if (id.x >= pn.size.x) {
        return;
    }
    out[id.x] = id.x + 1u;
}

@compute @workgroup_size(16, 4)
fn area(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= pn.size.xy)) {
        return;
    }
    out[id.y * pn.size.x + id.x] += 1u;
}
";

#[test]
fn a_range_dispatch_touches_exactly_the_first_n_elements() {
    let Some(gpu) = gpu() else { return };
    let program = Compute::compile(&gpu, "ranges", RANGES, &[]).unwrap();
    let entries: Vec<_> = program.entries().collect();
    assert_eq!(entries, [("line", Target::Grid), ("area", Target::Grid)]);
    assert_eq!(program.buffers().len(), 2);
    assert_eq!(program.buffers(), [BufferSlot::ReadWrite; 2]);
    let out = Buffer::of::<u32>(&gpu, 1000, "out").unwrap();
    let seen = Buffer::of::<u32>(&gpu, 1, "seen").unwrap();
    let input = plain(&none);
    let line = |n| Step { entry: "line", out: Out::range(n), input: &input };
    program.run_with(&gpu, &[&out, &seen], &[line(100)]).unwrap();
    let v: Vec<u32> = out.read(&gpu).unwrap();
    assert!(v[..100].iter().enumerate().all(|(i, x)| *x == i as u32 + 1));
    assert!(v[100..].iter().all(|x| *x == 0), "beyond the count");
    assert_eq!(seen.read::<u32>(&gpu).unwrap(), [100]);
    // A count of 0 dispatches nothing.
    out.clear(&gpu).unwrap();
    program.run_with(&gpu, &[&out, &seen], &[line(0)]).unwrap();
    assert!(out.read::<u32>(&gpu).unwrap().iter().all(|x| *x == 0));
    assert_eq!(seen.read::<u32>(&gpu).unwrap(), [100]);

    // A grid of sides that are no multiples of the workgroup's: each cell once.
    let area = Step { entry: "area", out: Out::grid(37, 11), input: &input };
    program.run_with(&gpu, &[&out, &seen], &[area]).unwrap();
    let v: Vec<u32> = out.read(&gpu).unwrap();
    assert!(v[..37 * 11].iter().all(|x| *x == 1));
    assert!(v[37 * 11..].iter().all(|x| *x == 0));

    // An entry point of a grid writes no picture.
    let pic = Tex::new_storage(&gpu, 8, 8, "pic").unwrap();
    let e = program
        .run_with(&gpu, &[&out, &seen], &[Step { entry: "area", out: Out::Picture(&pic), input: &input }])
        .err()
        .unwrap();
    assert!(e.0.contains("Grid"), "{e}");
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

const STEPS: &str = "
@group(2) @binding(0) var<storage, read_write> out: array<f32>;
@group(2) @binding(1) var<storage, read_write> total: array<f32>;

@compute @workgroup_size(1)
fn mark(@builtin(global_invocation_id) id: vec3u) {
    out[params.k] = params.value;
}

// Reads what the steps before wrote.
@compute @workgroup_size(1)
fn sum(@builtin(global_invocation_id) id: vec3u) {
    total[0] = total[0] + out[params.k];
}
";

#[test]
fn every_step_gets_its_own_uniforms_and_sees_the_steps_before() {
    let Some(gpu) = gpu() else { return };
    let params = [ParamSpec::int("k", 0), ParamSpec::float("value", 0.0)];
    let program = Compute::compile(&gpu, "steps", STEPS, &params).unwrap();
    // More steps than one block of uniform memory holds.
    let n = 2 * (crate::deferred::BLOCK / (3 * gpu.layouts().uniform_align)) as usize + 7;
    let out = Buffer::of::<f32>(&gpu, n as u64, "out").unwrap();
    let total = Buffer::of::<f32>(&gpu, 1, "total").unwrap();
    let values: Vec<_> = (0..n)
        .map(|k| {
            move |name: &str| match name {
                "k" => Some(Value::Int(k as i64)),
                "value" => Some(Value::Float((k % 50) as f64 * 0.5)),
                _ => None,
            }
        })
        .collect();
    let inputs: Vec<Dispatch<'_>> = values.iter().map(|v| plain(v)).collect();
    // Each step's dispatch twice in a row: the uniforms are packed once and still reach both.
    let steps: Vec<Step<'_>> = inputs
        .iter()
        .flat_map(|input| {
            [
                Step { entry: "mark", out: Out::range(1), input },
                Step { entry: "sum", out: Out::range(1), input },
            ]
        })
        .collect();
    let mut batch = crate::Batch::new(&gpu, "steps");
    program.encode_with(&mut batch, &[&out, &total], &steps).unwrap();
    drop(batch);
    let v: Vec<f32> = out.read(&gpu).unwrap();
    for (k, x) in v.iter().enumerate() {
        assert_eq!(*x, (k % 50) as f32 * 0.5, "step {k}");
    }
    let expected: f32 = v.iter().sum();
    assert_eq!(total.read::<f32>(&gpu).unwrap(), [expected]);
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

const ATOMICS: &str = "
@group(2) @binding(0) var<storage, read> values: array<i32>;
@group(2) @binding(1) var<storage, read_write> sums: array<atomic<i32>>;
@group(2) @binding(2) var<storage, read_write> counts: array<atomic<u32>>;
@group(2) @binding(3) var<storage, read_write> taken: array<i32>;

@compute @workgroup_size(64)
fn scatter(@builtin(global_invocation_id) id: vec3u) {
    let i = id.x;
    if (i >= pn.size.x) {
        return;
    }
    let cell = i % arrayLength(&sums);
    atomicAdd(&sums[cell], values[i]);
    atomicAdd(&counts[cell], 1u);
}

// Takes the sums out and leaves zero behind, as a grid update does.
@compute @workgroup_size(64)
fn take(@builtin(global_invocation_id) id: vec3u) {
    let i = id.x;
    if (i >= pn.size.x) {
        return;
    }
    taken[i] = atomicLoad(&sums[i]);
    atomicStore(&sums[i], 0);
}
";

#[test]
fn atomic_sums_of_many_into_few_cells_are_exact() {
    let Some(gpu) = gpu() else { return };
    let program = Compute::compile(&gpu, "atomics", ATOMICS, &[]).unwrap();
    let rw = BufferSlot::ReadWrite;
    assert_eq!(program.buffers(), [BufferSlot::Read, rw, rw, rw]);
    let n = 100_000usize;
    let cells = 7usize;
    let mut rng = 99u32;
    let values: Vec<i32> = (0..n)
        .map(|_| {
            rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
            (rng >> 16) as i32 - 32768
        })
        .collect();
    let mut sums = vec![0i32; cells];
    let mut counts = vec![0u32; cells];
    for (i, v) in values.iter().enumerate() {
        sums[i % cells] += v;
        counts[i % cells] += 1;
    }
    let vb = Buffer::of::<i32>(&gpu, n as u64, "values").unwrap();
    vb.write(&gpu, 0, &values).unwrap();
    let sb = Buffer::of::<i32>(&gpu, cells as u64, "sums").unwrap();
    let cb = Buffer::of::<u32>(&gpu, cells as u64, "counts").unwrap();
    let tb = Buffer::of::<i32>(&gpu, cells as u64, "taken").unwrap();
    let input = plain(&none);
    let buffers = [&vb, &sb, &cb, &tb];
    // Twice: the second scatter starts from the zeros the first take left.
    for _ in 0..2 {
        program
            .run_with(
                &gpu,
                &buffers,
                &[
                    Step { entry: "scatter", out: Out::range(n as u32), input: &input },
                    Step { entry: "take", out: Out::range(cells as u32), input: &input },
                ],
            )
            .unwrap();
        assert_eq!(tb.read::<i32>(&gpu).unwrap(), sums);
        assert_eq!(sb.read::<i32>(&gpu).unwrap(), vec![0; cells]);
    }
    assert_eq!(cb.read::<u32>(&gpu).unwrap(), counts.iter().map(|c| 2 * c).collect::<Vec<_>>());
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

#[test]
fn bindings_and_dispatches_that_cannot_work_are_readable_errors() {
    let Some(gpu) = gpu() else { return };
    let compile = |source: &str| Compute::compile(&gpu, "wrong.wgsl", source, &[]).err().unwrap().0;
    let entry = "\n@compute @workgroup_size(64) fn f(@builtin(global_invocation_id) id: vec3u) {}\n";

    // A texture where a buffer belongs: the line, the name, what to write instead.
    let e = compile(&format!("\n\n@group(2) @binding(0) var t: texture_2d<f32>;{entry}"));
    assert!(e.starts_with("wrong.wgsl:3: `t` is a texture or sampler;"), "{e}");
    assert!(e.contains("var<storage, read>"), "{e}");
    let e = compile(&format!("@group(3) @binding(0) var<storage, read> a: array<u32>;{entry}"));
    assert!(e.contains("@group(3)"), "{e}");
    let e = compile(&format!("@group(2) @binding(1) var<storage, read> a: array<u32>;{entry}"));
    assert!(e.contains("binding 0 is missing"), "{e}");
    // As many as the device binds: one too many beside the prelude's own.
    let most = gpu.device().limits().max_storage_buffers_per_shader_stage;
    let all: String = (0..most)
        .map(|i| format!("@group(2) @binding({i}) var<storage, read> b{i}: array<u32>;\n"))
        .collect();
    let e = compile(&format!("{all}{entry}"));
    assert!(e.contains(&format!("at most {}", most - 1)), "{e}");
    let many: String =
        (0..10).map(|i| format!("@group(2) @binding({i}) var<uniform> u{i}: vec4f;\n")).collect();
    let e = compile(&format!("{many}{entry}"));
    let most = gpu.device().limits().max_uniform_buffers_per_shader_stage;
    assert!(e.contains("10 uniforms") && e.contains(&format!("at most {}", most - 3)), "{e}");
    // An atomic in a buffer the shader may only read, and plain WGSL errors, keep naga's lines.
    let e = compile(
        "@group(2) @binding(0) var<storage, read> a: array<atomic<i32>>;\n\
         @compute @workgroup_size(64) fn f(@builtin(global_invocation_id) id: vec3u) {\n\
             atomicAdd(&a[id.x], 1);\n\
         }\n",
    );
    assert!(e.contains("wrong.wgsl:"), "{e}");
    let e = compile(&format!(
        "@group(2) @binding(0) var<storage, read_write> a: array<u32>;\nfn g() {{ nope; }}{entry}"
    ));
    assert!(e.contains("wrong.wgsl:2"), "{e}");

    // At dispatch: the wrong number of buffers, one smaller than the shader's type, too many
    // workgroups. Constants read the same from a storage buffer and from a uniform.
    for (declared, slot) in [("var<storage, read>", BufferSlot::Read), ("var<uniform>", BufferSlot::Uniform)]
    {
        let source = format!(
            "
struct C {{ a: vec4f, b: vec4f }}
@group(2) @binding(0) {declared} c: C;
@group(2) @binding(1) var<storage, read_write> out: array<f32>;
@compute @workgroup_size(64) fn f(@builtin(global_invocation_id) id: vec3u) {{
    if (id.x < pn.size.x) {{
        out[id.x] = c.b.x;
    }}
}}
"
        );
        let program = Compute::compile(&gpu, "sizes.wgsl", &source, &[]).unwrap();
        assert_eq!(program.buffers(), [slot, BufferSlot::ReadWrite]);
        let small = Buffer::of::<f32>(&gpu, 4, "small").unwrap();
        let out = Buffer::of::<f32>(&gpu, 64, "out").unwrap();
        let input = plain(&none);
        let step = |n| [Step { entry: "f", out: Out::range(n), input: &input }];
        let e = program.run_with(&gpu, &[&out], &step(64)).err().unwrap();
        assert!(e.0.contains("2 buffers") && e.0.contains("1 were given"), "{e}");
        assert!(program.run_with(&gpu, &[&small, &out], &step(64)).is_err());
        let e = program.run_with(&gpu, &[&out, &out], &step(u32::MAX)).err().unwrap();
        assert!(e.0.contains("workgroups"), "{e}");
        // With buffers that fit, it runs.
        let constants = Buffer::of::<f32>(&gpu, 8, "constants").unwrap();
        constants.write(&gpu, 4, &[3.5f32]).unwrap();
        program.run_with(&gpu, &[&constants, &out], &step(10)).unwrap();
        let v: Vec<f32> = out.read(&gpu).unwrap();
        assert!(v[..10].iter().all(|x| *x == 3.5) && v[10..].iter().all(|x| *x == 0.0));
        if slot == BufferSlot::Uniform {
            // Larger than a uniform binds.
            let most = gpu.device().limits().max_uniform_buffer_binding_size;
            let big = Buffer::new(&gpu, most + 16, "big").unwrap();
            let e = program.run_with(&gpu, &[&big, &out], &step(10)).err().unwrap();
            assert!(e.0.contains("a uniform of"), "{e}");
        }
    }
}
