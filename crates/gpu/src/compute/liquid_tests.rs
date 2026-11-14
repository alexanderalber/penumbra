//! A 2D MLS-MPM liquid on compute programs with storage buffers (`liquid_test.wgsl`): the
//! check that buffers, atomics, range and grid dispatches and per-step uniforms carry a real
//! simulation. A dam of water breaks in a closed room and comes to rest as a flat pool.

use std::time::Instant;

use penumbra_core::{ParamSpec, Value};

use crate::testing::gpu;
use crate::{AdapterChoice, Batch, Buffer, Compute, Dispatch, Gpu, Out, Step};

const SOURCE: &str = include_str!("liquid_test.wgsl");

const ROOM_W: f32 = 4.0;
const ROOM_H: f32 = 3.0;
const DT: f32 = 1.0 / 60.0;
/// Solid cells around the room.
const PAD: usize = 3;
/// Fluid area in m^2: the dam is a square of 1 m, the pool a quarter of a metre high.
const AREA: f32 = 1.0;
/// Floats per particle: position, velocity, two columns of the affine matrix, density, distance.
const P: usize = 10;

fn params() -> Vec<ParamSpec> {
    vec![ParamSpec::float("t", 0.0)]
}

struct Sim {
    program: Compute,
    u: Buffer,
    parts: Buffer,
    ga: Buffer,
    gf: Buffer,
    solid: Buffer,
    gw: usize,
    gh: usize,
    dx: f32,
    spacing: f32,
    substeps: usize,
    count: usize,
}

impl Sim {
    /// `n` particles in a dam at the left wall, `gridw` cells across the room, `substeps` per
    /// frame (0: from the sound speed), `damp` the share of its velocity the water loses per
    /// second.
    fn new(gpu: &Gpu, n: usize, gridw: usize, substeps: usize, damp: f32) -> Sim {
        let program = Compute::compile(gpu, "liquid_test.wgsl", SOURCE, &params()).unwrap();
        let (c, gamma, cfl, vref) = (12.0f32, 4.0f32, 0.7f32, 11.0f32);
        let spacing = (AREA / n as f32).sqrt();
        let dx = ROOM_W / gridw as f32;
        let ppc = dx * dx / (spacing * spacing);
        let gw = (ROOM_W / dx).round() as usize + 2 * PAD + 1;
        let gh = (ROOM_H / dx).round() as usize + 2 * PAD + 1;
        let substeps = if substeps > 0 { substeps } else { (DT * c.max(vref) / (cfl * dx)).ceil() as usize };
        let dt = DT / substeps as f32;
        let vmax = 0.9 * dx / dt;
        // Fixed point: the largest sums must fit 31 bits.
        let max_mass = 8.0 * ppc.max(1.0);
        let mscale = (2.0f32.powi(30) / max_mass).floor();
        let pscale = (2.0f32.powi(30) / (max_mass * vmax)).floor();
        let f = f32::to_bits;
        #[rustfmt::skip]
        let u: Vec<u32> = vec![
            gw as u32, gh as u32,
            f(dx), f(1.0 / dx), f(dt), f(c * c / gamma), f(gamma),
            f(0.002), // viscosity
            f(ppc), f(mscale), f(pscale), f(vmax),
            f(0.0),   // friction
            f(0.25),  // boundary band
            f(0.0), f(-9.81),
            f(0.3),   // push
            f(2.0),   // rmax
            f(damp),
            f(0.0),   // negative pressure
            f(0.0),   // bulk viscosity
            f(1.0),   // ghost
        ];
        let cells = (gw * gh) as u64;
        let ub = Buffer::of::<u32>(gpu, u.len() as u64, "u").unwrap();
        ub.write(gpu, 0, &u).unwrap();
        let parts = Buffer::of::<[f32; P]>(gpu, n as u64, "particles").unwrap();
        let ga = Buffer::of::<[i32; 3]>(gpu, cells, "grid fixed").unwrap();
        let gf = Buffer::of::<[f32; 4]>(gpu, cells, "grid float").unwrap();
        let solid = Buffer::of::<[f32; 4]>(gpu, cells, "solid").unwrap();

        // The room's walls as a distance field, negative inside.
        let mut field = Vec::with_capacity(gw * gh);
        for j in 0..gh {
            for i in 0..gw {
                let p = [(i as f32 - PAD as f32) * dx, (j as f32 - PAD as f32) * dx];
                let s = p[0].min(ROOM_W - p[0]).min(p[1]).min(ROOM_H - p[1]);
                field.push([s, 0.0, 0.0, 0.0]);
            }
        }
        solid.write(gpu, 0, &field).unwrap();

        // The dam: a jittered lattice, `spacing` apart, from the floor and the left wall.
        let w = AREA.sqrt();
        let cols = (w / spacing).floor().max(1.0) as usize;
        let pad = PAD as f32 * dx;
        let mut rng = 12345u32;
        let mut rand = || {
            rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
            (rng >> 8) as f32 / (1u32 << 24) as f32 - 0.5
        };
        let mut v = Vec::with_capacity(n);
        for k in 0..n {
            let (i, j) = (k % cols, k / cols);
            let x = (i as f32 + 0.5 + 0.3 * rand()) * spacing;
            let y = (j as f32 + 0.5 + 0.3 * rand()) * spacing;
            let mut p = [0.0; P];
            p[0] = x + pad;
            p[1] = y + pad;
            p[9] = 1.0;
            v.push(p);
        }
        parts.write(gpu, 0, &v).unwrap();
        Sim { program, u: ub, parts, ga, gf, solid, gw, gh, dx, spacing, substeps, count: n }
    }

    /// Records one frame: three dispatches per substep, each substep with its own `t`.
    fn frame(&self, gpu: &Gpu) {
        let dt = DT / self.substeps as f32;
        let values: Vec<_> = (0..self.substeps)
            .map(|k| move |n: &str| (n == "t").then_some(Value::Float(f64::from(k as f32 * dt))))
            .collect();
        let inputs: Vec<Dispatch<'_>> = values
            .iter()
            .map(|param| Dispatch { volumes: &[], pictures: &[], data: None, seconds: 0.0, frame: 0, param })
            .collect();
        let range = Out::range(self.count as u32);
        let grid = Out::grid(self.gw as u32, self.gh as u32);
        let mut steps = Vec::with_capacity(3 * self.substeps);
        for input in &inputs {
            steps.push(Step { entry: "p2g", out: range, input });
            steps.push(Step { entry: "grid_update", out: grid, input });
            steps.push(Step { entry: "g2p", out: range, input });
        }
        let mut batch = Batch::new(gpu, "liquid");
        let buffers = [&self.u, &self.parts, &self.ga, &self.gf, &self.solid];
        self.program.encode_with(&mut batch, &buffers, &steps).unwrap();
        batch.submit();
    }

    fn run(&self, gpu: &Gpu, frames: usize) {
        for f in 0..frames {
            self.frame(gpu);
            // Keeps the queue short on a software adapter.
            if f % 30 == 29 {
                gpu.wait(std::time::Duration::from_secs(120)).unwrap();
            }
        }
    }

    /// The particles, positions in the room's coordinates.
    fn particles(&self, gpu: &Gpu) -> Vec<[f32; P]> {
        let pad = PAD as f32 * self.dx;
        let mut v: Vec<[f32; P]> = self.parts.read(gpu).unwrap();
        for p in &mut v {
            p[0] -= pad;
            p[1] -= pad;
        }
        v
    }
}

struct Report {
    nan: usize,
    outside: usize,
    mean_speed: f32,
    max_speed: f32,
    pool_height: f32,
    pool_expected: f32,
}

fn report(parts: &[[f32; P]], spacing: f32, dx: f32) -> Report {
    let mut r =
        Report { nan: 0, outside: 0, mean_speed: 0.0, max_speed: 0.0, pool_height: 0.0, pool_expected: 0.0 };
    let e = 0.5 * dx;
    let mut speeds = 0.0f64;
    let cols = 80;
    let mut top = vec![0f32; cols];
    let mut pool = 0usize;
    for p in parts {
        if !p.iter().take(4).all(|x| x.is_finite()) {
            r.nan += 1;
            continue;
        }
        if p[0] < -e || p[0] > ROOM_W + e || p[1] < -e || p[1] > ROOM_H + e {
            r.outside += 1;
        }
        let speed = (p[2] * p[2] + p[3] * p[3]).sqrt();
        speeds += f64::from(speed);
        r.max_speed = r.max_speed.max(speed);
        if p[1] < 0.6 && p[0] >= 0.0 && p[0] < ROOM_W {
            pool += 1;
            let c = (((p[0] / ROOM_W) * cols as f32) as usize).min(cols - 1);
            top[c] = top[c].max(p[1] + 0.5 * spacing);
        }
    }
    r.mean_speed = (speeds / parts.len().max(1) as f64) as f32;
    top.sort_by(f32::total_cmp);
    r.pool_height = top[cols / 2];
    r.pool_expected = pool as f32 * spacing * spacing / ROOM_W;
    r
}

#[test]
fn the_fixtures_constants_match_what_the_test_writes() {
    // `U` as the shader lays it out: the words `Sim::new` writes, in that order.
    let full =
        format!("{SOURCE}\n{}{}{}", super::PRELUDE, crate::pass::SHARED, crate::pass::params_wgsl(&params()));
    let (size, members) = crate::testing::wgsl_struct(&full, "U");
    let names = [
        "grid", "dx", "inv_dx", "dt", "c2g", "gamma", "visc", "ppc", "mscale", "pscale", "vmax", "friction",
        "bc_band", "gravity", "push", "rmax", "damp", "pneg", "bulk", "ghost",
    ];
    let mut offset = 0;
    for name in names {
        assert_eq!(crate::testing::member(&members, name).offset, offset, "{name}");
        offset += if name == "grid" || name == "gravity" { 8 } else { 4 };
    }
    assert_eq!(size, offset);
    assert_eq!(crate::testing::wgsl_struct(&full, "P").0, (P * 4) as u32);
}

/// A dam break of 4,096 particles on a grid 128 cells across comes to rest as a flat pool of the
/// height its area gives, without a particle leaving the room. A second of this simulation takes
/// WARP about 2 seconds.
#[test]
fn a_dam_breaks_and_comes_to_rest_as_a_flat_pool() {
    let Some(gpu) = gpu() else { return };
    let sim = Sim::new(&gpu, 4096, 128, 0, 0.5);
    let seconds = 15.0;
    let started = Instant::now();
    sim.run(&gpu, (seconds / DT).round() as usize);
    let parts = sim.particles(&gpu);
    let r = report(&parts, sim.spacing, sim.dx);
    eprintln!(
        "dam break, {} particles, grid {}x{}, {} substeps, {seconds} s in {:.1} s: nan {} outside {} mean speed \
         {:.4} max {:.3} pool {:.4} m (expected {:.4})",
        sim.count,
        sim.gw,
        sim.gh,
        sim.substeps,
        started.elapsed().as_secs_f64(),
        r.nan,
        r.outside,
        r.mean_speed,
        r.max_speed,
        r.pool_height,
        r.pool_expected
    );
    assert_eq!(parts.len(), 4096);
    assert_eq!(r.nan, 0);
    assert_eq!(r.outside, 0);
    assert!(
        r.mean_speed < 0.025 && r.max_speed < 0.25,
        "still moving: mean {} m/s, max {}",
        r.mean_speed,
        r.max_speed
    );
    assert!(r.pool_expected > 0.24, "particles above the pool: expected height {}", r.pool_expected);
    let error = (r.pool_height - r.pool_expected).abs() / r.pool_expected;
    assert!(error < 0.04, "pool {} m against {} m", r.pool_height, r.pool_expected);
    assert_eq!(gpu.errors().count, 0, "{:?}", gpu.errors().last);
}

/// Two runs of the same scene give the same bits: the fixed-point sums do not depend on the order
/// in which the particles add into a cell.
#[test]
fn a_run_repeats_bit_for_bit() {
    let Some(gpu) = gpu() else { return };
    let run = || {
        let sim = Sim::new(&gpu, 2048, 64, 0, 0.5);
        let start: Vec<u32> = sim.parts.read(&gpu).unwrap();
        sim.run(&gpu, 30);
        let end: Vec<u32> = sim.parts.read(&gpu).unwrap();
        assert_ne!(start, end, "nothing moved");
        (end, sim.gf.read::<u32>(&gpu).unwrap())
    };
    let (a, b) = (run(), run());
    assert!(a.0 == b.0, "the particles differ");
    assert!(a.1 == b.1, "the grids differ");
}

/// 65,536 particles on a grid 512 cells across, 37 substeps, on the hardware GPU. Prints the wall
/// time per frame (recording, submission and the GPU's work) and the time per frame of frames
/// recorded back to back.
#[test]
#[ignore = "times the liquid on the hardware GPU: cargo test -p penumbra-gpu --release liquid_timing -- --ignored --nocapture"]
fn liquid_timing_at_the_default_size() {
    let gpu = Gpu::headless(AdapterChoice::Hardware).unwrap();
    eprintln!("{}", gpu.name());
    let sim = Sim::new(&gpu, 65536, 512, 37, 0.5);
    let frames = 240;
    let mut wall = Vec::with_capacity(frames);
    let mut record = Vec::with_capacity(frames);
    for f in 0..frames {
        let t0 = Instant::now();
        sim.frame(&gpu);
        let recorded = t0.elapsed();
        gpu.wait(std::time::Duration::from_secs(10)).unwrap();
        // The first frames carry pipeline compilation.
        if f >= 10 {
            wall.push(t0.elapsed().as_secs_f64() * 1e3);
            record.push(recorded.as_secs_f64() * 1e3);
        }
    }
    let stats = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        (mean, v[(v.len() - 1) * 95 / 100], v[v.len() - 1])
    };
    let (mean, p95, max) = stats(&mut wall);
    let (rmean, rp95, rmax) = stats(&mut record);
    // Frames back to back: the CPU records the next while the GPU works, so the time per frame
    // is the GPU's, as long as it takes longer than the recording.
    let t0 = Instant::now();
    for _ in 0..frames {
        sim.frame(&gpu);
    }
    gpu.wait(std::time::Duration::from_secs(10)).unwrap();
    let throughput = t0.elapsed().as_secs_f64() * 1e3 / frames as f64;
    let r = report(&sim.particles(&gpu), sim.spacing, sim.dx);
    eprintln!(
        "grid {}x{}, {} substeps: wall ms per frame mean {mean:.3} p95 {p95:.3} max {max:.3}; recording on the CPU \
         mean {rmean:.3} p95 {rp95:.3} max {rmax:.3}; back to back {throughput:.3} ms per frame; nan {} outside {} mean speed {:.3}",
        sim.gw, sim.gh, sim.substeps, r.nan, r.outside, r.mean_speed
    );
}
