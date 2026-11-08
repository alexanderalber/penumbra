//! Capturing this machine's own desktop. Ignored by default: they need a desktop session, and
//! what they see depends on the machine. `cargo test -p penumbra-screen -- --ignored` runs them.

use std::process::{Child, Command};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use penumbra_gpu::{AdapterChoice, Gpu, Tex};

use crate::{Capture, Options, Target, sources};

const OPTIONS: Options = Options { cursor: false, border: false };

/// Takes frames until one arrives or `timeout` passes.
fn first_frame(
    gpu: &Gpu,
    capture: &mut Capture,
    woken: &mpsc::Receiver<()>,
    timeout: Duration,
) -> Option<Tex> {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        woken.recv_timeout(Duration::from_millis(50)).ok();
        match capture.take(gpu) {
            Ok(Some(t)) => return Some(t),
            Ok(None) => {}
            Err(e) => panic!("{e}"),
        }
    }
    None
}

fn capture(gpu: &Gpu, target: Target) -> (Capture, mpsc::Receiver<()>) {
    let (tx, rx) = mpsc::channel();
    let mut capture = Capture::new(gpu).unwrap();
    capture
        .start(target, OPTIONS, move || {
            tx.send(()).ok();
        })
        .unwrap();
    (capture, rx)
}

/// The primary monitor at its size, with something other than black on it.
fn captures_the_primary_monitor(choice: AdapterChoice) {
    let gpu = Gpu::headless(choice).unwrap();
    let list = sources();
    let primary = list.monitors.iter().find(|m| m.primary).expect("a primary monitor");
    let (mut capture, woken) = capture(&gpu, Target::Monitor(String::new()));
    let tex = first_frame(&gpu, &mut capture, &woken, Duration::from_secs(5))
        .unwrap_or_else(|| panic!("no frame: {:?}", capture.status()));
    assert_eq!(tex.size(), (primary.width, primary.height));
    let pixels = gpu.read(&tex).unwrap();
    let lit = pixels.data.iter().filter(|p| p[0] > 0.01 || p[1] > 0.01 || p[2] > 0.01).count();
    assert!(lit > 0, "the capture is black");
    assert!(pixels.data.iter().all(|p| (p[3] - 1.0).abs() < 1e-3), "the capture is not opaque");
    assert_eq!(capture.status().capturing.as_deref(), Some(primary.id.as_str()));
    assert_eq!(capture.status().warning, None, "border");
}

#[test]
#[ignore = "needs a desktop session"]
fn captures_the_primary_monitor_on_warp() {
    captures_the_primary_monitor(AdapterChoice::Software);
}

#[test]
#[ignore = "needs a desktop session and a GPU"]
fn captures_the_primary_monitor_on_the_gpu() {
    captures_the_primary_monitor(AdapterChoice::Hardware);
}

struct Kill(Child);

impl Drop for Kill {
    fn drop(&mut self) {
        self.0.kill().ok();
    }
}

/// A window found by its title, followed when it closes and comes back.
#[test]
#[ignore = "needs a desktop session; opens a window"]
fn a_window_is_captured_and_found_again() {
    let gpu = Gpu::headless(AdapterChoice::Hardware).unwrap();
    let title = format!("penumbra capture test {}", std::process::id());
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms; $f = New-Object Windows.Forms.Form; $f.Text = '{title}'; \
         $f.BackColor = 'Red'; $f.Width = 400; $f.Height = 300; $t = New-Object Windows.Forms.Timer; \
         $t.Interval = 300; $script:n = 0; $t.add_Tick({{ $script:n++; if ($script:n -eq 5) {{ $f.Width = 700; \
         $f.Height = 500 }}; if ($script:n % 2) {{ $f.BackColor = 'Blue' }} else {{ $f.BackColor = 'Red' }} }}); \
         $t.Start(); [Windows.Forms.Application]::Run($f)"
    );
    let open = || Kill(Command::new("powershell").args(["-NoProfile", "-Command", &script]).spawn().unwrap());
    let window = open();
    let (mut capture, woken) = capture(&gpu, Target::Window(title.to_uppercase()));
    let tex = first_frame(&gpu, &mut capture, &woken, Duration::from_secs(15))
        .unwrap_or_else(|| panic!("no frame: {:?}", capture.status()));
    let (w, h) = tex.size();
    assert!(w >= 300 && h >= 200, "{w}x{h}");
    // The form's middle is red or blue, whichever it shows at the moment.
    let pixels = gpu.read(&tex).unwrap();
    let p = pixels.data[(h / 2 * w + w / 2) as usize];
    assert!((p[0] > 0.8 || p[2] > 0.8) && p[1] < 0.05, "the middle is {p:?}");
    assert_eq!(capture.status().capturing.as_deref(), Some(title.as_str()));
    // The window grows after a moment; the picture follows.
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        let grown = first_frame(&gpu, &mut capture, &woken, Duration::from_millis(200));
        if grown.is_some_and(|t| t.size().0 > w + 100 && t.size().1 > h + 100) {
            break;
        }
        assert!(
            Instant::now() < end,
            "the picture did not follow the window's size: {:?} {:?}",
            capture.status(),
            sources().windows.iter().find(|x| x.title == title)
        );
    }
    drop(window);
    let end = Instant::now() + Duration::from_secs(5);
    while capture.status().error.is_none() {
        assert!(Instant::now() < end, "the closed window was not noticed");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(capture.status().error.unwrap_or_default().contains("closed"));
    let _again = open();
    let tex = first_frame(&gpu, &mut capture, &woken, Duration::from_secs(15))
        .unwrap_or_else(|| panic!("no frame after reopening: {:?}", capture.status()));
    assert!(tex.size().0 >= 300);
    assert_eq!(capture.status().error, None);
}

#[test]
fn a_missing_window_is_an_error_on_the_status() {
    let Ok(gpu) = Gpu::headless(AdapterChoice::Software) else { return };
    let (capture, woken) = capture(&gpu, Target::Window("no window is called this 7f3a9c".into()));
    woken.recv_timeout(Duration::from_secs(5)).ok();
    let end = Instant::now() + Duration::from_secs(5);
    while capture.status().error.is_none() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
    let error = capture.status().error.unwrap_or_default();
    assert!(error.contains("no window") || error.contains("cannot capture the screen"), "{error}");
}

fn median(mut v: Vec<Duration>) -> f64 {
    v.sort();
    v.get(v.len() / 2).map_or(f64::NAN, |d| d.as_secs_f64() * 1000.0)
}

/// What a frame costs, printed: the CPU time of `take` and the time until the GPU finished it,
/// against an empty submission. A borderless 1280x720 form (1920x1080 pixels at 150 % scaling)
/// changes colour every timer tick, so both the window and the monitor send frames.
#[test]
#[ignore = "measures; needs a desktop session and a GPU"]
fn cost_of_a_frame() {
    let gpu = Gpu::headless(AdapterChoice::Hardware).unwrap();
    eprintln!("GPU: {}", gpu.name());
    let title = format!("penumbra capture cost {}", std::process::id());
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms; $f = New-Object Windows.Forms.Form; $f.Text = '{title}'; \
         $f.FormBorderStyle = 'None'; $f.ClientSize = New-Object Drawing.Size(1280, 720); $f.TopMost = $true; \
         $f.StartPosition = 'Manual'; $f.Location = New-Object Drawing.Point(0, 0); \
         $t = New-Object Windows.Forms.Timer; $t.Interval = 10; $script:n = 0; \
         $t.add_Tick({{ $script:n++; $f.BackColor = [Drawing.Color]::FromArgb(255, $script:n % 256, 64, 128) }}); \
         $t.Start(); [Windows.Forms.Application]::Run($f)"
    );
    let _window = Kill(Command::new("powershell").args(["-NoProfile", "-Command", &script]).spawn().unwrap());
    let end = Instant::now() + Duration::from_secs(15);
    while !sources().windows.iter().any(|w| w.title == title) {
        assert!(Instant::now() < end, "the form did not open");
        std::thread::sleep(Duration::from_millis(50));
    }
    let finish = |gpu: &Gpu| {
        let encoder = gpu.device().create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        gpu.submit([encoder.finish()]);
        gpu.settle(Duration::from_secs(10));
    };
    for target in [Target::Window(title.clone()), Target::Monitor(String::new())] {
        let (mut capture, woken) = capture(&gpu, target.clone());
        first_frame(&gpu, &mut capture, &woken, Duration::from_secs(10)).expect("a first frame");
        let (mut take, mut total, mut base) = (Vec::new(), Vec::new(), Vec::new());
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(3) {
            woken.recv_timeout(Duration::from_millis(100)).ok();
            let t0 = Instant::now();
            let got = capture.take(&gpu).unwrap();
            let t1 = Instant::now();
            finish(&gpu);
            if got.is_some() {
                take.push(t1 - t0);
                total.push(t0.elapsed());
                let t2 = Instant::now();
                finish(&gpu);
                base.push(t2.elapsed());
            }
        }
        let frames = take.len();
        eprintln!(
            "{target:?} {:?}: {frames} frames in 3 s, take (CPU) median {:.3} ms, take until the GPU is done {:.3} ms, \
             an empty submission until done {:.3} ms",
            capture.status().size,
            median(take),
            median(total),
            median(base),
        );
    }
}
