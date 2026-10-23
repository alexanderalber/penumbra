//! Structured light against synthetic camera pictures: a known mapping from camera to projector
//! pixels (a homography, a box standing nearer with parallax, a gentle warp), the patterns
//! rendered through it with shadows, gloss, dark and bright surfaces, ambient light, blur and
//! noise, decoded and compared with the truth.

use crate::Homography;
use crate::scan::{Decoder, Options, Pattern, Patterns, Sample, Scan, Status, bits, from_gray, fuse, gray};

/// A tiny deterministic generator, so the synthetic data is the same on every run.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 40) as f32) / (1u64 << 24) as f32
    }

    /// Roughly normal (sum of four uniforms), standard deviation `sd`.
    fn gauss(&mut self, sd: f32) -> f32 {
        let s: f32 = (0..4).map(|_| self.next() - 0.5).sum();
        s * sd * 3f32.sqrt()
    }
}

/// Per camera pixel: the projector point it truly sees (None where not reached) and whether the
/// pixel is clean enough to judge.
type Truth = Vec<(Option<[f32; 2]>, bool)>;

/// What the camera sees at one point of its picture.
#[derive(Clone, Copy)]
struct Surface {
    /// The projector pixel lighting it (fractional), `None` in shadow.
    proj: Option<[f32; 2]>,
    albedo: [f32; 3],
    /// A mirror-like highlight: the projector point it reflects and its strength.
    gloss: Option<([f32; 2], f32)>,
}

/// A synthetic setup: camera and projector sizes, the scene, and the disturbances.
struct Setup {
    projector: (u32, u32),
    camera: (u32, u32),
    /// Camera picture point to what is there.
    scene: Box<dyn Fn(f32, f32) -> Surface>,
    /// Projector brightness on a white surface.
    gain: f32,
    ambient: f32,
    noise: f32,
    /// Gaussian blur sigma in camera pixels (0: none).
    blur: f32,
    /// Subsamples per camera pixel and axis.
    supersample: u32,
}

/// A homography from camera to projector: the projector picture seen slightly from the side,
/// filling most of the camera picture.
fn view(camera: (u32, u32), projector: (u32, u32)) -> Homography {
    let (cw, ch) = (f64::from(camera.0), f64::from(camera.1));
    let (pw, ph) = (f64::from(projector.0), f64::from(projector.1));
    let pairs = [
        ([0.08 * cw, 0.10 * ch], [0.0, 0.0]),
        ([0.95 * cw, 0.04 * ch], [pw, 0.0]),
        ([0.90 * cw, 0.97 * ch], [pw, ph]),
        ([0.04 * cw, 0.88 * ch], [0.0, ph]),
    ];
    Homography::from_pairs(&pairs).unwrap()
}

fn map(h: &Homography, x: f32, y: f32) -> [f32; 2] {
    let p = h.apply([f64::from(x), f64::from(y)]).unwrap();
    [p[0] as f32, p[1] as f32]
}

impl Setup {
    /// A wall seen through a homography with a gentle warp: nothing else.
    fn plain(projector: (u32, u32), camera: (u32, u32)) -> Self {
        let h = view(camera, projector);
        let (cw, ch) = (camera.0 as f32, camera.1 as f32);
        Self {
            projector,
            camera,
            scene: Box::new(move |x, y| {
                let mut p = map(&h, x, y);
                p[0] += 1.5 * (y / ch * 3.0).sin();
                p[1] += 1.5 * (x / cw * 2.0).cos();
                Surface { proj: Some(p), albedo: [0.7; 3], gloss: None }
            }),
            gain: 0.8,
            ambient: 0.0,
            noise: 0.0,
            blur: 0.0,
            supersample: 3,
        }
    }

    /// The wall with a box standing in front of it: the box's face is shifted in the projector
    /// picture by the parallax between lens and camera (`shift` projector pixels), and casts a
    /// shadow on the wall beside it. Inside the box: a dark book, a bright book, a glossy patch.
    fn objects(projector: (u32, u32), camera: (u32, u32), shift: f32) -> Self {
        let h = view(camera, projector);
        let (cw, ch) = (camera.0 as f32, camera.1 as f32);
        let face = move |x: f32, y: f32| x > 0.30 * cw && x < 0.62 * cw && y > 0.25 * ch && y < 0.75 * ch;
        let shadow = move |x: f32, y: f32| {
            x >= 0.62 * cw && x < 0.62 * cw + 0.06 * cw && y > 0.25 * ch && y < 0.75 * ch
        };
        let dark = move |x: f32, y: f32| x > 0.33 * cw && x < 0.40 * cw && y > 0.3 * ch && y < 0.7 * ch;
        let bright = move |x: f32, y: f32| x > 0.42 * cw && x < 0.48 * cw && y > 0.3 * ch && y < 0.7 * ch;
        let (gx, gy, gr) = (0.55 * cw, 0.5 * ch, 0.04 * cw);
        Self {
            projector,
            camera,
            scene: Box::new(move |x, y| {
                let mut p = map(&h, x, y);
                if shadow(x, y) {
                    return Surface { proj: None, albedo: [0.7; 3], gloss: None };
                }
                let mut albedo = [0.7; 3];
                let mut gloss = None;
                if face(x, y) {
                    p[0] += shift;
                    p[1] -= 0.3 * shift;
                    albedo = [0.6, 0.55, 0.5];
                    if dark(x, y) {
                        albedo = [0.04, 0.05, 0.08];
                    }
                    if bright(x, y) {
                        albedo = [0.95, 0.95, 0.9];
                    }
                    let r2 = ((x - gx).powi(2) + (y - gy).powi(2)) / (gr * gr);
                    if r2 < 4.0 {
                        // A mirror: the reflected projector point runs the other way.
                        let mirrored = map(&h, 2.0 * gx - x, 2.0 * gy - y);
                        gloss = Some((mirrored, 4.0 * (-r2).exp()));
                    }
                }
                Surface { proj: Some(p), albedo, gloss }
            }),
            gain: 0.8,
            ambient: 0.0,
            noise: 0.0,
            blur: 0.0,
            supersample: 3,
        }
    }

    /// The truth per camera pixel: the projector point at its centre, `None` where the projector
    /// does not reach it; and whether the pixel is clean (all subsamples reached or none, and no
    /// depth edge or highlight inside it).
    fn truth(&self) -> Truth {
        let (cw, ch) = self.camera;
        let s = self.supersample.max(1);
        let mut out = Vec::with_capacity((cw * ch) as usize);
        for y in 0..ch {
            for x in 0..cw {
                let centre = (self.scene)(x as f32 + 0.5, y as f32 + 0.5);
                let inside = |p: [f32; 2]| {
                    p[0] >= 0.0
                        && p[1] >= 0.0
                        && p[0] < self.projector.0 as f32
                        && p[1] < self.projector.1 as f32
                };
                let reach = |sf: &Surface| sf.proj.filter(|p| inside(*p));
                let mut reached = 0;
                let mut clean = true;
                for j in 0..s {
                    for i in 0..s {
                        let sf = (self.scene)(
                            x as f32 + (i as f32 + 0.5) / s as f32,
                            y as f32 + (j as f32 + 0.5) / s as f32,
                        );
                        if reach(&sf).is_some() {
                            reached += 1;
                        }
                        if let (Some(p), Some(c)) = (reach(&sf), reach(&centre))
                            && ((p[0] - c[0]).abs() > 2.0 || (p[1] - c[1]).abs() > 2.0)
                        {
                            clean = false;
                        }
                        if sf.gloss.is_some_and(|(_, g)| g > 0.02) {
                            clean = false;
                        }
                        // A border between dark and bright albedo blurs the contrast, not the code.
                    }
                }
                let all = reached == s * s;
                clean &= all || reached == 0;
                // Near the projector's edge the stripe at the border may read either way.
                if let Some(c) = reach(&centre) {
                    clean &= c[0] > 1.0
                        && c[1] > 1.0
                        && c[0] < self.projector.0 as f32 - 1.0
                        && c[1] < self.projector.1 as f32 - 1.0;
                }
                out.push((if all { reach(&centre) } else { None }, clean));
            }
        }
        // Blur carries light a few pixels across the edge between reached and not reached: those
        // pixels are neither.
        let r = (3.0 * self.blur).ceil() as i64;
        if r > 0 {
            let (w, h) = (cw as i64, ch as i64);
            let reached: Vec<bool> = out.iter().map(|(t, _)| t.is_some()).collect();
            let edge: Vec<bool> = (0..w * h)
                .map(|i| {
                    let (x, y) = (i % w, i / w);
                    let me = reached[i as usize];
                    (-r..=r).any(|dy| {
                        (-r..=r).any(|dx| {
                            let (nx, ny) = (x + dx, y + dy);
                            nx >= 0 && ny >= 0 && nx < w && ny < h && reached[(ny * w + nx) as usize] != me
                        })
                    })
                })
                .collect();
            for (o, e) in out.iter_mut().zip(edge) {
                o.1 &= !e;
            }
        }
        out
    }

    /// The camera picture of `pattern`: linear RGBA, clipped at 1.
    fn render(&self, patterns: &Patterns, pattern: Pattern, rng: &mut Lcg) -> Vec<[f32; 4]> {
        let (cw, ch) = self.camera;
        let (pw, ph) = self.projector;
        let s = self.supersample.max(1);
        let lit = |p: [f32; 2]| -> f32 {
            if p[0] < 0.0 || p[1] < 0.0 || p[0] >= pw as f32 || p[1] >= ph as f32 {
                return 0.0;
            }
            if patterns.lit(pattern, p[0] as u32, p[1] as u32) { self.gain } else { 0.0 }
        };
        let mut img = vec![[0.0f32; 3]; (cw * ch) as usize];
        for y in 0..ch {
            for x in 0..cw {
                let mut acc = [0.0f32; 3];
                for j in 0..s {
                    for i in 0..s {
                        let sf = (self.scene)(
                            x as f32 + (i as f32 + 0.5) / s as f32,
                            y as f32 + (j as f32 + 0.5) / s as f32,
                        );
                        let light = self.ambient + sf.proj.map_or(0.0, lit);
                        let spec = sf.gloss.map_or(0.0, |(p, g)| g * lit(p));
                        for (a, al) in acc.iter_mut().zip(sf.albedo) {
                            *a += al * light + spec;
                        }
                    }
                }
                img[(y * cw + x) as usize] = acc.map(|v| v / (s * s) as f32);
            }
        }
        if self.blur > 0.0 {
            img = blur(&img, cw as usize, ch as usize, self.blur);
        }
        img.iter()
            .map(|c| {
                let n = |v: f32, r: &mut Lcg| (v + r.gauss(self.noise)).clamp(0.0, 1.0);
                [n(c[0], rng), n(c[1], rng), n(c[2], rng), 1.0]
            })
            .collect()
    }

    fn scan(&self, options: &Options) -> (Scan, Truth) {
        let patterns = Patterns::new(self.projector.0, self.projector.1).unwrap();
        let mut d = Decoder::new(patterns, self.camera.0, self.camera.1).unwrap();
        let mut rng = Lcg(7);
        for k in 0..patterns.count() {
            let img = self.render(&patterns, patterns.get(k).unwrap(), &mut rng);
            d.add(k, &img).unwrap();
        }
        (d.finish(options).unwrap(), self.truth())
    }
}

/// Separable Gaussian blur, edges held.
fn blur(img: &[[f32; 3]], w: usize, h: usize, sigma: f32) -> Vec<[f32; 3]> {
    let r = (3.0 * sigma).ceil() as isize;
    let k: Vec<f32> = (-r..=r).map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp()).collect();
    let sum: f32 = k.iter().sum();
    let pass = |src: &[[f32; 3]], dx: isize, dy: isize| -> Vec<[f32; 3]> {
        let mut out = vec![[0.0; 3]; w * h];
        for y in 0..h as isize {
            for x in 0..w as isize {
                let mut acc = [0.0; 3];
                for (t, kv) in (-r..=r).zip(&k) {
                    let sx = (x + t * dx).clamp(0, w as isize - 1);
                    let sy = (y + t * dy).clamp(0, h as isize - 1);
                    let v = src[(sy * w as isize + sx) as usize];
                    for c in 0..3 {
                        acc[c] += v[c] * kv / sum;
                    }
                }
                out[(y * w as isize + x) as usize] = acc;
            }
        }
        out
    };
    pass(&pass(img, 1, 0), 0, 1)
}

/// How a decode compares with the truth over the clean pixels.
#[derive(Debug)]
struct Report {
    /// Clean pixels the projector reaches.
    reached: usize,
    /// Of those, kept.
    kept: usize,
    /// Of the kept, within 1 and 2 projector pixels on both axes.
    within1: usize,
    within2: usize,
    worst: f32,
    /// Clean pixels the projector does not reach that were kept anyway.
    false_kept: usize,
    unreached: usize,
}

impl Report {
    fn kept_share(&self) -> f32 {
        self.kept as f32 / self.reached.max(1) as f32
    }
    fn within1_share(&self) -> f32 {
        self.within1 as f32 / self.kept.max(1) as f32
    }
    fn within2_share(&self) -> f32 {
        self.within2 as f32 / self.kept.max(1) as f32
    }
}

fn compare(scan: &Scan, truth: &[(Option<[f32; 2]>, bool)], only: impl Fn(usize) -> bool) -> Report {
    let mut r =
        Report { reached: 0, kept: 0, within1: 0, within2: 0, worst: 0.0, false_kept: 0, unreached: 0 };
    for (i, ((t, clean), p)) in truth.iter().zip(&scan.points).enumerate() {
        if !clean || !only(i) {
            continue;
        }
        match (t, p) {
            (Some(t), Some(p)) => {
                r.reached += 1;
                r.kept += 1;
                let e = (t[0] - p[0]).abs().max((t[1] - p[1]).abs());
                r.worst = r.worst.max(e);
                r.within1 += usize::from(e <= 1.0);
                r.within2 += usize::from(e <= 2.0);
            }
            (Some(_), None) => r.reached += 1,
            (None, Some(_)) => {
                r.unreached += 1;
                r.false_kept += 1;
            }
            (None, None) => r.unreached += 1,
        }
    }
    r
}

#[test]
fn gray_codes_round_trip_and_neighbours_differ_in_one_bit() {
    for v in 0..5000u32 {
        assert_eq!(from_gray(gray(v)), v);
        assert_eq!((gray(v) ^ gray(v + 1)).count_ones(), 1);
    }
    assert_eq!(bits(1920), 11);
    assert_eq!(bits(1080), 11);
    assert_eq!(bits(2048), 11);
    assert_eq!(bits(2049), 12);
    assert_eq!(bits(1), 1);
}

#[test]
fn a_full_hd_scan_is_46_pictures_in_a_fixed_order() {
    let p = Patterns::new(1920, 1080).unwrap();
    assert_eq!(p.count(), 46);
    assert_eq!(p.get(0), Some(Pattern::White));
    assert_eq!(p.get(1), Some(Pattern::Black));
    assert_eq!(p.get(2), Some(Pattern::Columns { bit: 0, inverse: false }));
    assert_eq!(p.get(3), Some(Pattern::Columns { bit: 0, inverse: true }));
    assert_eq!(p.get(24), Some(Pattern::Rows { bit: 0, inverse: false }));
    assert_eq!(p.get(45), Some(Pattern::Rows { bit: 10, inverse: true }));
    assert_eq!(p.get(46), None);
    for k in 0..46 {
        assert_eq!(p.index(p.get(k).unwrap()), Some(k));
    }
    assert_eq!(p.get(45).unwrap().name(), "row 10 inverse");
    // The coarsest column bit splits the code range in half; the finest stripes are 2 wide.
    let col0 = Pattern::Columns { bit: 0, inverse: false };
    assert!(!p.lit(col0, 1023, 0) && p.lit(col0, 1024, 0));
    let fine = Pattern::Columns { bit: 10, inverse: false };
    let row: Vec<bool> = (0..8).map(|x| p.lit(fine, x, 0)).collect();
    assert_eq!(row, [false, true, true, false, false, true, true, false]);
    assert!(Patterns::new(0, 10).is_err());
    assert!(Patterns::new(20000, 10).is_err());
}

#[test]
fn the_decoder_refuses_wrong_sizes_and_missing_pictures() {
    let p = Patterns::new(16, 8).unwrap();
    assert!(Decoder::new(p, 0, 4).is_err());
    assert!(Decoder::new(p, 1 << 13, 1 << 13).is_err());
    let mut d = Decoder::new(p, 4, 4).unwrap();
    assert!(d.add(0, &[[1.0; 4]; 15]).unwrap_err().contains("15 pixels"));
    assert!(d.add(p.count(), &[[1.0; 4]; 16]).is_err());
    d.add(0, &[[1.0; 4]; 16]).unwrap();
    d.add(2, &[[1.0; 4]; 16]).unwrap();
    let missing = d.missing();
    assert_eq!(missing.len(), p.count() - 2);
    assert!(missing.contains(&1) && missing.contains(&3) && !missing.contains(&2));
    let e = d.finish(&Options::default()).unwrap_err();
    assert!(e.contains("missing") && e.contains("black"), "{e}");
    let bad = Options { min_confidence: 2.0, ..Options::default() };
    assert!(d.finish(&bad).unwrap_err().contains("min_confidence"));
}

#[test]
fn pictures_in_any_order_decode_alike_and_a_repeated_bit_starts_over() {
    let setup = Setup::plain((64, 32), (48, 40));
    let patterns = Patterns::new(64, 32).unwrap();
    let mut rng = Lcg(1);
    let imgs: Vec<_> =
        (0..patterns.count()).map(|k| setup.render(&patterns, patterns.get(k).unwrap(), &mut rng)).collect();
    let mut a = Decoder::new(patterns, 48, 40).unwrap();
    for (k, img) in imgs.iter().enumerate() {
        a.add(k, img).unwrap();
    }
    let mut b = Decoder::new(patterns, 48, 40).unwrap();
    // Backwards, with a wrong picture for bit 2 first, then that bit again.
    b.add(7, &imgs[0]).unwrap();
    b.add(6, &imgs[0]).unwrap();
    for (k, img) in imgs.iter().enumerate().rev() {
        b.add(k, img).unwrap();
    }
    b.add(6, &imgs[6]).unwrap();
    assert!(b.missing() == [7], "{:?}", b.missing());
    b.add(7, &imgs[7]).unwrap();
    let o = Options::default();
    assert_eq!(a.finish(&o).unwrap().points, b.finish(&o).unwrap().points);
}

#[test]
fn a_clean_wall_decodes_to_within_a_pixel() {
    let setup = Setup::plain((256, 160), (240, 180));
    let (scan, truth) = setup.scan(&Options::default());
    let r = compare(&scan, &truth, |_| true);
    eprintln!("clean: {r:?}, stats {:?}", scan.stats);
    assert!(r.kept_share() > 0.99, "{r:?}");
    assert!(r.within1_share() > 0.99, "{r:?}");
    assert_eq!(r.false_kept, 0, "{r:?}");
    assert!(r.unreached > 1000, "the camera sees beyond the projector's picture: {r:?}");
}

#[test]
fn ambient_light_noise_and_blur_cost_little() {
    let mut setup = Setup::plain((256, 160), (240, 180));
    setup.ambient = 0.25;
    setup.noise = 0.01;
    setup.blur = 0.8;
    let (scan, truth) = setup.scan(&Options::default());
    let r = compare(&scan, &truth, |_| true);
    eprintln!("ambient 0.25, noise 0.01, blur 0.8: {r:?}, stats {:?}", scan.stats);
    assert!(r.kept_share() > 0.95, "{r:?}");
    assert!(r.within1_share() > 0.97, "{r:?}");
    assert!(r.within2_share() > 0.995, "{r:?}");
    assert_eq!(r.false_kept, 0, "{r:?}");
}

#[test]
fn a_camera_coarser_than_the_projector_still_maps_to_a_few_pixels() {
    // The finest stripes are 2 projector pixels, under one camera pixel here.
    let mut setup = Setup::plain((512, 320), (200, 150));
    setup.noise = 0.005;
    setup.blur = 0.6;
    let (scan, truth) = setup.scan(&Options::default());
    let r = compare(&scan, &truth, |_| true);
    eprintln!("coarse camera: {r:?}, stats {:?}", scan.stats);
    assert!(scan.stats.resolved[0] < scan.stats.bits[0], "{:?}", scan.stats);
    assert!(r.kept_share() > 0.9, "{r:?}");
    let within4 = {
        let mut n = 0;
        for ((t, clean), p) in truth.iter().zip(&scan.points) {
            if let (Some(t), Some(p), true) = (t, p, clean) {
                n += usize::from((t[0] - p[0]).abs().max((t[1] - p[1]).abs()) <= 4.0);
            }
        }
        n as f32 / r.kept.max(1) as f32
    };
    assert!(within4 > 0.98, "{within4} {r:?}");
}

/// Splits the objects setup into its regions for the per-disturbance numbers.
fn region(setup: &Setup, i: usize) -> &'static str {
    let (cw, ch) = (setup.camera.0 as f32, setup.camera.1 as f32);
    let (x, y) = ((i % setup.camera.0 as usize) as f32 + 0.5, (i / setup.camera.0 as usize) as f32 + 0.5);
    let in_box = y > 0.25 * ch && y < 0.75 * ch;
    if in_box && x >= 0.62 * cw && x < 0.68 * cw {
        "shadow"
    } else if in_box && x > 0.33 * cw && x < 0.40 * cw && y > 0.3 * ch && y < 0.7 * ch {
        "dark"
    } else if in_box && x > 0.42 * cw && x < 0.48 * cw && y > 0.3 * ch && y < 0.7 * ch {
        "bright"
    } else if ((x - 0.55 * cw).powi(2) + (y - 0.5 * ch).powi(2)).sqrt() < 0.08 * cw {
        "gloss"
    } else if in_box && x > 0.30 * cw && x < 0.62 * cw {
        "face"
    } else {
        "wall"
    }
}

#[test]
fn objects_with_shadow_gloss_dark_and_bright_surfaces() {
    let mut setup = Setup::objects((256, 160), (240, 180), 9.0);
    setup.ambient = 0.1;
    setup.noise = 0.008;
    setup.blur = 0.7;
    let (scan, truth) = setup.scan(&Options::default());
    eprintln!("objects stats {:?}", scan.stats);
    for name in ["wall", "face", "dark", "bright", "shadow"] {
        let r = compare(&scan, &truth, |i| region(&setup, i) == name);
        eprintln!(
            "{name}: kept {:.3} within1 {:.3} within2 {:.3} worst {:.1} false_kept {} / {}",
            r.kept_share(),
            r.within1_share(),
            r.within2_share(),
            r.worst,
            r.false_kept,
            r.unreached
        );
        match name {
            "shadow" => {
                assert!(r.unreached > 100 && r.false_kept == 0, "{name}: {r:?}");
            }
            // On the dark book the contrast is 0.04 against noise of 0.008: the finest bits flip
            // now and then, costing a second pixel.
            "dark" => {
                assert!(r.kept_share() > 0.9, "{name}: {r:?}");
                assert!(r.within1_share() > 0.85, "{name}: {r:?}");
                assert!(r.within2_share() > 0.99, "{name}: {r:?}");
            }
            _ => {
                assert!(r.kept_share() > 0.9, "{name}: {r:?}");
                assert!(r.within1_share() > 0.97, "{name}: {r:?}");
                assert!(r.within2_share() > 0.99, "{name}: {r:?}");
            }
        }
    }
    // The highlight: what is kept there must be right (the clipped core and the outliers go).
    let mut kept = 0;
    let mut wrong = 0;
    let mut total = 0;
    for (i, p) in scan.points.iter().enumerate() {
        if region(&setup, i) != "gloss" {
            continue;
        }
        let t = (setup.scene)((i % 240) as f32 + 0.5, (i / 240) as f32 + 0.5);
        if t.gloss.is_none_or(|(_, g)| g < 0.05) {
            continue;
        }
        total += 1;
        if let (Some(p), Some(t)) = (p, t.proj) {
            kept += 1;
            wrong += usize::from((t[0] - p[0]).abs().max((t[1] - p[1]).abs()) > 2.0);
        }
    }
    eprintln!("gloss: {total} pixels, {kept} kept, {wrong} of them off by more than 2");
    assert!(total > 100);
    assert!((wrong as f32) < 0.03 * total as f32, "gloss: {wrong} wrong of {total}");
    assert!(scan.stats.count(Status::Clipped) > 0);
}

#[test]
fn the_inverse_points_back_and_shows_the_scene_from_the_projector() {
    let mut setup = Setup::objects((256, 160), (240, 180), 9.0);
    setup.noise = 0.005;
    setup.blur = 0.5;
    let (scan, _) = setup.scan(&Options::default());
    let inv = scan.inverse(32);
    let [measured, filled, empty] = inv.counts();
    eprintln!("inverse: measured {measured}, filled {filled}, empty {empty}");
    assert_eq!(measured + filled + empty, 256 * 160);
    assert!(measured > 256 * 160 / 2);
    // Every measured projector pixel: the camera point it names maps back onto it.
    let mut good = 0;
    let mut n = 0;
    for (j, (p, s)) in inv.points.iter().zip(&inv.samples).enumerate() {
        if *s != Sample::Measured {
            continue;
        }
        let p = p.unwrap();
        let back = (setup.scene)(p[0], p[1]).proj;
        let Some(back) = back else { continue };
        n += 1;
        let (px, py) = ((j % 256) as f32 + 0.5, (j / 256) as f32 + 0.5);
        good += usize::from((back[0] - px).abs() <= 1.5 && (back[1] - py).abs() <= 1.5);
    }
    eprintln!("inverse: {good} of {n} measured pixels map back within 1.5");
    assert!(good as f32 > 0.97 * n as f32, "{good} of {n}");
    // The shadow behind the box is a hole in camera space but filled in projector space; the
    // dark book shows dark in the scene.
    let map = inv.projector_map();
    let scene = inv.scene();
    assert_eq!(map.len(), 256 * 160);
    assert!(scene.iter().zip(&inv.samples).all(|(c, s)| (*s == Sample::Empty) == (c[3] == 0.0)));
    let dark_cam = [0.365 * 240.0, 0.5 * 180.0];
    let dark_proj = (setup.scene)(dark_cam[0], dark_cam[1]).proj.unwrap();
    let j = dark_proj[1] as usize * 256 + dark_proj[0] as usize;
    assert!(scene[j][0] < 0.15 && inv.samples[j] == Sample::Measured, "{:?}", scene[j]);
    // Nothing is filled from further than asked.
    let none = scan.inverse(0);
    assert_eq!(none.counts()[1], 0);
}

#[test]
fn camera_map_and_quality_mark_what_is_not_valid() {
    let setup = Setup::objects((128, 80), (120, 90), 5.0);
    let (scan, _) = setup.scan(&Options::default());
    let map = scan.camera_map();
    let q = scan.quality();
    for ((m, p), (st, qc)) in map.iter().zip(&scan.points).zip(scan.status.iter().zip(&q)) {
        match p {
            Some(p) => {
                assert_eq!(*st, Status::Valid);
                assert!((m[0] * 128.0 - p[0]).abs() < 1e-3 && m[2] > 0.0);
            }
            None => {
                assert_ne!(*st, Status::Valid);
                assert_eq!(m[0], -1.0);
                assert!(!(qc[0] == qc[1] && qc[1] == qc[2]) || qc[0] == 0.0, "{qc:?}");
            }
        }
    }
}

/// The speed of a full decode: a 1920 x 1080 projector seen by a 1920 x 1080 camera. Run with
/// `cargo test --release -p penumbra-calib -- --ignored --nocapture full_hd`.
#[test]
#[ignore]
fn full_hd_decode_timing() {
    let mut setup = Setup::plain((1920, 1080), (1920, 1080));
    setup.supersample = 1;
    setup.noise = 0.005;
    let patterns = Patterns::new(1920, 1080).unwrap();
    let mut d = Decoder::new(patterns, 1920, 1080).unwrap();
    let mut rng = Lcg(3);
    let mut adding = std::time::Duration::ZERO;
    for k in 0..patterns.count() {
        let img = setup.render(&patterns, patterns.get(k).unwrap(), &mut rng);
        let t = std::time::Instant::now();
        d.add(k, &img).unwrap();
        adding += t.elapsed();
    }
    let t = std::time::Instant::now();
    let scan = d.finish(&Options::default()).unwrap();
    let finish = t.elapsed();
    let t = std::time::Instant::now();
    let inv = scan.inverse(32);
    let inverse = t.elapsed();
    let t = std::time::Instant::now();
    let (a, b, c) = (scan.camera_map(), inv.projector_map(), inv.scene());
    let maps = t.elapsed();
    let truth = setup.truth();
    let r = compare(&scan, &truth, |_| true);
    eprintln!(
        "full HD: add {adding:?} (46 pictures), finish {finish:?}, inverse {inverse:?}, maps {maps:?} ({} {} {}); {r:?}; {:?}; inverse {:?}",
        a.len(),
        b.len(),
        c.len(),
        scan.stats,
        inv.counts()
    );
    assert!(r.within1_share() > 0.95, "{r:?}");
}

/// 8-bit sRGB PPM of linear RGB(A) pixels.
fn ppm(path: &std::path::Path, w: u32, h: u32, px: impl Iterator<Item = [f32; 3]>) {
    let enc = |c: f32| {
        let c = c.clamp(0.0, 1.0);
        let s = if c <= 0.0031308 { 12.92 * c } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
        (s * 255.0).round() as u8
    };
    let mut bytes = format!("P6\n{w} {h}\n255\n").into_bytes();
    for p in px {
        bytes.extend(p.map(enc));
    }
    std::fs::write(path, bytes).unwrap();
}

/// Writes the camera pictures of a full-size scan of the objects setup (shadow, gloss, dark and
/// bright books, ambient light, noise, blur) as `NN.ppm` into `$PENUMBRA_SCAN_OUT`, plus
/// `truth/scene.ppm`, the scene as the projector sees it from the known mapping. For looking at
/// the decode through `scan.decode`; run with
/// `PENUMBRA_SCAN_OUT=dir cargo test --release -p penumbra-calib -- --ignored write_synthetic`.
#[test]
#[ignore]
fn write_synthetic_captures() {
    let Some(out) = std::env::var_os("PENUMBRA_SCAN_OUT") else { return };
    let out = std::path::PathBuf::from(out);
    std::fs::create_dir_all(out.join("truth")).unwrap();
    let (proj, cam) = ((1920, 1080), (1920, 1080));
    let mut setup = Setup::objects(proj, cam, 40.0);
    setup.supersample = 2;
    setup.ambient = 0.1;
    setup.noise = 0.008;
    setup.blur = 0.8;
    let patterns = Patterns::new(proj.0, proj.1).unwrap();
    let mut rng = Lcg(11);
    for k in 0..patterns.count() {
        let img = setup.render(&patterns, patterns.get(k).unwrap(), &mut rng);
        ppm(&out.join(format!("{k:02}.ppm")), cam.0, cam.1, img.iter().map(|p| [p[0], p[1], p[2]]));
    }
    let mut scene = vec![[0.0f32; 3]; (proj.0 * proj.1) as usize];
    for y in 0..cam.1 {
        for x in 0..cam.0 {
            let sf = (setup.scene)(x as f32 + 0.5, y as f32 + 0.5);
            if let Some(p) = sf.proj
                && p[0] >= 0.0
                && p[1] >= 0.0
                && (p[0] as u32) < proj.0
                && (p[1] as u32) < proj.1
            {
                scene[(p[1] as u32 * proj.0 + p[0] as u32) as usize] =
                    sf.albedo.map(|a| a * (setup.gain + setup.ambient));
            }
        }
    }
    ppm(&out.join("truth/scene.ppm"), proj.0, proj.1, scene.into_iter());
}

/// The plain wall, but on a patch of the camera picture each camera pixel spans 8 projector
/// pixels across (a face seen at a grazing angle squeezes the stripes): the finest two column
/// bits average out there, while the rest of the picture resolves them.
fn squeezed() -> (Setup, impl Fn(f32, f32) -> bool + Copy) {
    let patch = |x: f32, y: f32| (60.0..140.0).contains(&x) && (50.0..120.0).contains(&y);
    let mut s = Setup::plain((256, 160), (240, 180));
    s.noise = 0.004;
    s.supersample = 8;
    let inner = Setup::plain((256, 160), (240, 180)).scene;
    s.scene = Box::new(move |x, y| {
        if !patch(x, y) {
            return inner(x, y);
        }
        let mut f = inner(x.floor() + 0.5, y);
        if let Some(p) = f.proj.as_mut() {
            p[0] += (x - x.floor() - 0.5) * 8.0;
        }
        f
    });
    (s, patch)
}

#[test]
fn a_patch_that_does_not_resolve_the_finest_bits_still_decodes() {
    let (setup, patch) = squeezed();
    let on_patch = |scan: &Scan, truth: &Truth| {
        let w = scan.camera.0 as usize;
        let (mut all, mut kept, mut far) = (0usize, 0usize, 0usize);
        for (i, p) in scan.points.iter().enumerate() {
            let (x, y) = ((i % w) as f32 + 0.5, (i / w) as f32 + 0.5);
            // Clear of the patch's edge, where the window sees both.
            if !patch(x - 8.0, y - 8.0) || !patch(x + 8.0, y + 8.0) {
                continue;
            }
            let Some((Some(t), _)) = truth.get(i) else { continue };
            all += 1;
            if let Some(p) = p {
                kept += 1;
                far += usize::from((p[0] - t[0]).abs() > 6.0 || (p[1] - t[1]).abs() > 2.0);
            }
        }
        (kept as f64 / all.max(1) as f64, far)
    };
    let (local, truth) = setup.scan(&Options::default());
    let (global, _) = setup.scan(&Options { window: 0, ..Options::default() });
    let (kept_local, far_local) = on_patch(&local, &truth);
    let (kept_global, _) = on_patch(&global, &truth);
    eprintln!(
        "on the patch: kept {kept_local:.2} judged locally, {kept_global:.2} over the picture; {far_local} off;          resolved {:?}",
        local.stats.resolved
    );
    assert!(kept_global < 0.5, "{kept_global}");
    assert!(kept_local > 0.9, "{kept_local}");
    assert_eq!(far_local, 0);
    // Off the patch both judge alike.
    let off = |scan: &Scan| {
        scan.points
            .iter()
            .enumerate()
            .filter(|(i, p)| {
                let w = scan.camera.0 as usize;
                let (x, y) = ((i % w) as f32 + 0.5, (i / w) as f32 + 0.5);
                p.is_some() && !patch(x + 10.0, y + 10.0) && !patch(x - 10.0, y - 10.0) && !patch(x, y)
            })
            .count()
    };
    assert!(off(&local).abs_diff(off(&global)) < 50, "{} {}", off(&local), off(&global));
}

#[test]
fn the_window_is_checked() {
    let bad = Options { window: crate::scan::MAX_WINDOW + 1, ..Options::default() };
    assert!(bad.check().is_err());
    assert!(Options { window: 0, ..Options::default() }.check().is_ok());
}

#[test]
fn a_bright_and_a_dim_scan_merge_where_each_decodes() {
    // Left a white surface the bright scan clips on, right a dark one the dim scan loses.
    let mut setup = Setup::plain((256, 160), (240, 180));
    setup.noise = 0.002;
    let inner = Setup::plain((256, 160), (240, 180)).scene;
    setup.scene = Box::new(move |x, y| {
        let mut f = inner(x, y);
        f.albedo = if x < 120.0 { [1.6; 3] } else { [0.04; 3] };
        f
    });
    let left = |scan: &Scan, i: usize| (i % scan.camera.0 as usize) < 120;
    setup.gain = 0.8;
    let (bright, truth) = setup.scan(&Options::default());
    setup.gain = 0.3;
    let (dim, _) = setup.scan(&Options::default());
    let share = |scan: &Scan, on_left: bool| {
        let r = compare(scan, &truth, |i| left(scan, i) == on_left);
        r.kept_share()
    };
    eprintln!(
        "bright: left {:.2}, right {:.2}; dim: left {:.2}, right {:.2}",
        share(&bright, true),
        share(&bright, false),
        share(&dim, true),
        share(&dim, false)
    );
    assert!(share(&bright, true) < 0.05 && share(&bright, false) > 0.95);
    assert!(share(&dim, true) > 0.95 && share(&dim, false) < 0.05);
    let clipped = bright.stats.count(Status::Clipped);

    let f = fuse(vec![bright.clone(), dim.clone()]).unwrap();
    let r = compare(&f.scan, &truth, |_| true);
    eprintln!("merged: {r:?}, taken {:?}, only {:?}, apart {:?}", f.taken, f.only, f.apart);
    assert!(r.kept_share() > 0.95, "{r:?}");
    assert!(r.within1_share() > 0.99, "{r:?}");
    assert_eq!(r.false_kept, 0);
    assert_eq!(f.valid, [bright.stats.count(Status::Valid), dim.stats.count(Status::Valid)]);
    assert_eq!(f.taken.iter().sum::<usize>(), f.scan.stats.count(Status::Valid));
    assert!(f.only[0] > 5000 && f.only[1] > 5000, "{:?}", f.only);
    // The bright scan's clipped pixels are the dim one's now; its colour stays.
    assert!(f.scan.stats.count(Status::Clipped) < clipped / 20, "{clipped} -> {:?}", f.scan.stats);
    assert_eq!(f.scan.color, bright.color);
    // Each alone merges to itself; mismatched sizes and nothing refuse.
    let alone = fuse(vec![dim.clone()]).unwrap();
    assert_eq!((alone.scan.points, alone.apart), (dim.points.clone(), None));
    assert!(fuse(Vec::new()).is_err());
    let other = Setup::plain((128, 160), (240, 180)).scan(&Options::default()).0;
    assert!(fuse(vec![dim, other]).unwrap_err().contains("cannot merge"));
}

/// Light thrown on by a wall decodes as if the projector lit the surface directly, at a
/// projector pixel nowhere near it: the decode's own epipolar geometry finds those pixels.
#[test]
fn pixels_off_the_geometry_are_dropped() {
    let (w, h) = (320usize, 240usize);
    let mut rng = Lcg(5);
    // Camera at the origin looking at a wall with a box in front; the projector shifted and
    // turned, at a projector of 640 x 480.
    let project = |x: f64, y: f64, z: f64| {
        let (c, s) = (0.25f64.cos(), 0.25f64.sin());
        let (px, pz) = (c * x + s * z - 0.6, -s * x + c * z);
        [320.0 + 500.0 * px / pz, 240.0 + 500.0 * y / pz]
    };
    let mut points: Vec<Option<[f32; 2]>> = vec![None; w * h];
    let mut status = vec![Status::Dark; w * h];
    let mut contrast = vec![0.0f32; w * h];
    let mut bounced = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let (rx, ry) = ((x as f64 + 0.5 - 160.0) / 250.0, (y as f64 + 0.5 - 120.0) / 250.0);
            // The box: nearer in the middle of the picture.
            let z = if (100..220).contains(&x) && (80..200).contains(&y) { 2.0 } else { 3.0 };
            let q = project(rx * z, ry * z, z);
            // The right quarter of the picture: lit only by light the wall throws on, decoded at
            // random coarse codes, dimmer.
            let wrong = x >= 240;
            let p = if wrong {
                [(rng.next() * 8.0).floor() * 80.0 + 40.0, (rng.next() * 8.0).floor() * 60.0 + 30.0]
            } else {
                [(q[0] as f32).floor() + 0.5, (q[1] as f32).floor() + 0.5]
            };
            points[i] = Some(p);
            status[i] = Status::Valid;
            contrast[i] = if wrong { 0.05 } else { 0.4 + 0.1 * rng.next() };
            bounced[i] = wrong;
        }
    }
    let dropped = crate::scan::drop_off_geometry(&mut points, &mut status, &contrast, w, 4.0);
    let kept_right = (0..w * h).filter(|i| !bounced[*i] && points[*i].is_some()).count();
    let right = bounced.iter().filter(|b| !**b).count();
    let kept_wrong = (0..w * h).filter(|i| bounced[*i] && points[*i].is_some()).count();
    let wrong = bounced.iter().filter(|b| **b).count();
    eprintln!("dropped {dropped}; kept {kept_right} of {right} right, {kept_wrong} of {wrong} wrong");
    assert_eq!(kept_right, right);
    assert!(kept_wrong * 20 < wrong, "{kept_wrong} of {wrong}");
    assert!(status.iter().zip(&points).all(|(s, p)| p.is_some() == (*s == Status::Valid)));
    assert!(status.iter().filter(|s| **s == Status::Inconsistent).count() == dropped);
    // Off: nothing dropped.
    let mut p2 = vec![Some([1.0f32, 1.0]); w * h];
    let mut s2 = vec![Status::Valid; w * h];
    assert_eq!(crate::scan::drop_off_geometry(&mut p2, &mut s2, &contrast, w, 0.0), 0);
}
