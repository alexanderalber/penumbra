//! Times the Open DMX USB from the host side: `cargo run -p penumbra-dmx --example probe
//! [PORT] [FRAMES]`. Lists the serial ports, opens PORT (default: the first FT232R), sends
//! FRAMES frames (default 200) back to back and prints how long break and write took.

use std::time::{Duration, Instant};

use penumbra_dmx::serial::{self, Line};

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    for p in serial::ports()? {
        println!("{p:?}");
    }
    let name = match args.first() {
        Some(n) => n.clone(),
        None => serial::find_open_dmx().ok_or("no FT232R found")?,
    };
    let frames: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(200);
    let mut line = Line::open(&name)?;
    println!("opened {}", line.name());
    let mut brk = Vec::new();
    let mut write = Vec::new();
    let mut period = Vec::new();
    let mut last: Option<Instant> = None;
    let start = Instant::now();
    for i in 0..frames {
        let slots: Vec<u8> = (0..512).map(|k| ((k + i) % 256) as u8).collect();
        let now = Instant::now();
        if let Some(l) = last {
            period.push(now - l);
        }
        last = Some(now);
        let t = line.send(&slots)?;
        brk.push(t.brk);
        write.push(t.write);
    }
    let total = start.elapsed();
    let show = |what: &str, v: &mut Vec<Duration>| {
        v.sort();
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        let mean = v.iter().map(|d| ms(*d)).sum::<f64>() / v.len().max(1) as f64;
        let at = |q: f64| v.get(((v.len() as f64 - 1.0) * q) as usize).map(|d| ms(*d)).unwrap_or(0.0);
        println!(
            "{what:7} min {:7.3}  median {:7.3}  mean {:7.3}  p99 {:7.3}  max {:7.3} ms",
            at(0.0),
            at(0.5),
            mean,
            at(0.99),
            at(1.0)
        );
    };
    show("break", &mut brk);
    show("write", &mut write);
    show("period", &mut period);
    println!(
        "{frames} frames in {:.3} s: {:.2} Hz",
        total.as_secs_f64(),
        frames as f64 / total.as_secs_f64()
    );
    Ok(())
}
