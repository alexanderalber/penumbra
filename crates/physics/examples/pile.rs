//! Times a pile: `cargo run --release -p penumbra-physics --example pile -- 1000 box`.
//! Prints the milliseconds a step of a sixtieth of a second takes, while the bodies fall and
//! once they lie in a heap.

use std::time::Instant;

use penumbra_physics::{Settings, Shape, Start, World};

fn main() {
    let mut args = std::env::args().skip(1);
    let count: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(1000);
    let shape = match args.next().as_deref() {
        Some("sphere") => Shape::Sphere { center: [0.0; 3], radius: 0.5 },
        Some("hull") => Shape::Hull {
            points: (0..400)
                .map(|k| {
                    // Points on a ball of 0.5 m, spread by the golden angle.
                    let z = 1.0 - 2.0 * (k as f32 + 0.5) / 400.0;
                    let r = (1.0 - z * z).sqrt();
                    let a = k as f32 * 2.399_963;
                    [0.5 * r * a.cos(), 0.5 * r * a.sin(), 0.5 * z]
                })
                .collect(),
        },
        _ => Shape::Box { center: [0.0; 3], half: [0.5; 3] },
    };
    let mut world = match World::new(shape, Settings::default()) {
        Ok(w) => w,
        Err(e) => return eprintln!("{e}"),
    };
    // A column ten bodies wide and deep, so they end in one heap.
    for k in 0..count {
        let (x, y, z) = (k % 10, (k / 10) % 10, k / 100);
        let start = Start {
            translate: [x as f32 * 1.2 + 0.1 * (z % 3) as f32, y as f32 * 1.2, 1.0 + z as f32 * 1.3],
            rotate_degrees: [(k * 37 % 90) as f32, (k * 53 % 90) as f32, 0.0],
            ..Start::default()
        };
        if let Err(e) = world.add(&start) {
            return eprintln!("{e}");
        }
    }
    for second in 0..10 {
        let (mut total, mut worst) = (0.0f64, 0.0f64);
        for _ in 0..60 {
            let t = Instant::now();
            world.step(1.0 / 60.0, 1);
            let ms = t.elapsed().as_secs_f64() * 1e3;
            total += ms;
            worst = worst.max(ms);
        }
        println!(
            "second {second}: {:.2} ms a step, {worst:.2} ms the slowest, {} of {} asleep",
            total / 60.0,
            world.asleep(),
            world.len()
        );
    }
}
