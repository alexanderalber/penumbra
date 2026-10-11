//! The projector calibration against synthetic data: a known projector, world points projected
//! through it, noise added, the parameters recovered.

use crate::{Camera, Intrinsics, Options, Pose, Start, calibrate};

/// A tiny deterministic generator, so the synthetic data is the same on every run.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }

    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next()
    }

    /// Roughly normal (sum of four uniforms), standard deviation `sd`.
    fn gauss(&mut self, sd: f64) -> f64 {
        let s: f64 = (0..4).map(|_| self.next() - 0.5).sum();
        s * sd * 3f64.sqrt()
    }
}

const SIZE: [u32; 2] = [1920, 1080];

/// A 1080p projector with a throw ratio of 1.4 and lens shift (the principal point near the
/// bottom edge), 4 m in front of an object at the origin, a little to the side and above. World
/// y points down, so "up" is -y.
fn truth() -> Camera {
    let pose = Pose::look_at([0.6, -1.0, -4.0], [0.0, 0.3, 0.0], [0.0, -1.0, 0.0]).unwrap();
    let mut intrinsics = Intrinsics::from_throw_ratio(SIZE, 1.4);
    intrinsics.principal = [975.0, 1010.0];
    Camera { intrinsics, pose }
}

/// Points on an object about 1.6 x 1.2 x 1 m around the origin (not on one plane).
fn object(rng: &mut Lcg, n: usize) -> Vec<[f64; 3]> {
    (0..n).map(|_| [rng.range(-0.8, 0.8), rng.range(-0.6, 0.6), rng.range(-0.5, 0.5)]).collect()
}

fn pairs(cam: &Camera, world: &[[f64; 3]], rng: &mut Lcg, noise: f64) -> Vec<([f64; 3], [f64; 2])> {
    world
        .iter()
        .map(|w| {
            let q = cam.project(*w).unwrap();
            (*w, [q[0] + rng.gauss(noise), q[1] + rng.gauss(noise)])
        })
        .collect()
}

fn dist3(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// The angle between two poses' rotations in degrees.
fn angle_deg(a: &Pose, b: &Pose) -> f64 {
    let d = a.rotation3() * b.rotation3().inverse();
    d.angle().to_degrees()
}

/// The largest picture distance between two cameras over a grid of test points on the object
/// (also points not used in the fit).
fn agreement(a: &Camera, b: &Camera) -> f64 {
    let mut worst = 0.0f64;
    for i in 0..5 {
        for j in 0..5 {
            for k in 0..3 {
                let w = [-0.8 + 0.4 * i as f64, -0.6 + 0.3 * j as f64, -0.5 + 0.5 * k as f64];
                let (p, q) = (a.project(w).unwrap(), b.project(w).unwrap());
                worst = worst.max((p[0] - q[0]).hypot(p[1] - q[1]));
            }
        }
    }
    worst
}

fn agreement_rms(a: &Camera, b: &Camera) -> f64 {
    let mut e = Vec::new();
    for i in 0..5 {
        for j in 0..5 {
            for k in 0..3 {
                let w = [-0.8 + 0.4 * i as f64, -0.6 + 0.3 * j as f64, -0.5 + 0.5 * k as f64];
                let (p, q) = (a.project(w).unwrap(), b.project(w).unwrap());
                e.push((p[0] - q[0]).hypot(p[1] - q[1]));
            }
        }
    }
    crate::camera::rms(&e)
}

#[test]
fn six_exact_pairs_recover_the_projector() {
    let cam = truth();
    let mut rng = Lcg(1);
    let data = pairs(&cam, &object(&mut rng, 6), &mut rng, 0.0);
    let s = calibrate(&data, SIZE, None, &Options::default()).unwrap();
    let k = &s.camera.intrinsics;
    assert!(s.rms < 1e-6, "{}", s.rms);
    assert!((k.focal[0] - cam.intrinsics.focal[0]).abs() < 1e-4, "{:?}", k.focal);
    assert!(
        (k.principal[0] - 975.0).abs() < 1e-4 && (k.principal[1] - 1010.0).abs() < 1e-4,
        "{:?}",
        k.principal
    );
    assert!(dist3(s.camera.position(), cam.position()) < 1e-7);
    assert!(angle_deg(&s.camera.pose, &cam.pose) < 1e-6);
    assert_eq!(s.start, Start::Dlt);
    assert!(s.converged);
}

#[test]
fn noisy_pairs_recover_the_projector() {
    // Half a pixel of noise (standard deviation) on 20 points, the way a careful user places
    // them; the projector's view of the object comes back within about a pixel.
    let cam = truth();
    let mut rng = Lcg(2);
    let data = pairs(&cam, &object(&mut rng, 20), &mut rng, 0.5);
    let s = calibrate(&data, SIZE, None, &Options::default()).unwrap();
    let k = &s.camera.intrinsics;
    // Two axes of 0.5 px each: about 0.7 px per point, less what the 9 parameters absorb.
    assert!(s.rms < 0.9, "rms {}", s.rms);
    assert!((k.focal[0] / cam.intrinsics.focal[0] - 1.0).abs() < 0.01, "{:?}", k.focal);
    assert!(
        (k.principal[0] - 975.0).abs() < 15.0 && (k.principal[1] - 1010.0).abs() < 15.0,
        "{:?}",
        k.principal
    );
    assert!(dist3(s.camera.position(), cam.position()) < 0.05, "{:?}", s.camera.position());
    assert!(angle_deg(&s.camera.pose, &cam.pose) < 0.3);
    // The fit explains the data at least as well as the truth does (the optimiser finds the
    // minimum); on the object the two agree to about a pixel, worst corner 2.5.
    assert!(s.rms <= cam.rms_error(&data));
    assert!(agreement_rms(&s.camera, &cam) < 1.0, "{}", agreement_rms(&s.camera, &cam));
    assert!(agreement(&s.camera, &cam) < 2.5, "{}", agreement(&s.camera, &cam));
    let spread = s.spread.unwrap();
    assert!(spread.focal[0] > 0.0 && spread.focal[0] < 30.0, "{spread:?}");
    assert_eq!(s.errors.len(), 20);
}

#[test]
fn many_seeds_many_poses() {
    // Property-style, deterministic: random projectors around a random object, 12 points with
    // one pixel of noise. The optimiser always finds a fit at least as good as the truth; on
    // the object the fit and the truth agree to 1 to 4 px RMS (12 points with free focal length
    // and principal point leave that much play), worst corner about 12 px.
    let (mut worst, mut worst_rms) = (0.0f64, 0.0f64);
    for seed in 0..40 {
        let mut rng = Lcg(100 + seed);
        let eye = [rng.range(-3.0, 3.0), rng.range(-2.0, 0.5), rng.range(-6.0, -2.5)];
        let pose =
            Pose::look_at(eye, [rng.range(-0.3, 0.3), rng.range(-0.2, 0.4), 0.0], [0.0, -1.0, 0.0]).unwrap();
        let mut intrinsics = Intrinsics::from_throw_ratio(SIZE, rng.range(0.8, 2.0));
        intrinsics.principal = [rng.range(900.0, 1020.0), rng.range(540.0, 1080.0)];
        let cam = Camera { intrinsics, pose };
        let data = pairs(&cam, &object(&mut rng, 12), &mut rng, 1.0);
        let s =
            calibrate(&data, SIZE, None, &Options::default()).unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        assert!(s.rms <= cam.rms_error(&data) + 1e-9, "seed {seed}: rms {}", s.rms);
        worst = worst.max(agreement(&s.camera, &cam));
        worst_rms = worst_rms.max(agreement_rms(&s.camera, &cam));
    }
    assert!(worst_rms < 4.0 && worst < 15.0, "worst disagreement {worst_rms} px RMS, {worst} px max");
}

#[test]
fn millimetres_far_from_the_origin() {
    // The same projector in millimetres, with the world origin 100 m away.
    let m = truth();
    let off = [100_000.0, -40_000.0, 250_000.0];
    let r = m.pose.rotation3();
    let t = nalgebra::Vector3::from(m.pose.translation) * 1000.0 - r * nalgebra::Vector3::from(off);
    let cam =
        Camera { intrinsics: m.intrinsics, pose: Pose { rotation: m.pose.rotation, translation: t.into() } };
    let mut rng = Lcg(3);
    let world: Vec<[f64; 3]> = object(&mut rng, 8)
        .iter()
        .map(|w| [w[0] * 1000.0 + off[0], w[1] * 1000.0 + off[1], w[2] * 1000.0 + off[2]])
        .collect();
    let data = pairs(&cam, &world, &mut rng, 0.0);
    let s = calibrate(&data, SIZE, None, &Options::default()).unwrap();
    assert!(s.rms < 1e-6, "{}", s.rms);
    assert!(dist3(s.camera.position(), cam.position()) < 1e-3);
    assert!((s.camera.intrinsics.focal[0] - cam.intrinsics.focal[0]).abs() < 1e-3);
}

/// A projector on a flat wall (z = 0), principal point at the centre.
fn wall_camera() -> Camera {
    let pose = Pose::look_at([1.0, -0.5, -3.5], [0.0, 0.0, 0.0], [0.0, -1.0, 0.0]).unwrap();
    Camera { intrinsics: Intrinsics::from_throw_ratio(SIZE, 1.2), pose }
}

fn wall(rng: &mut Lcg, n: usize) -> Vec<[f64; 3]> {
    (0..n).map(|_| [rng.range(-1.2, 1.2), rng.range(-0.7, 0.7), 0.0]).collect()
}

#[test]
fn a_flat_wall_with_a_fixed_principal_point() {
    let cam = wall_camera();
    let opts = Options { fix_principal_point: true, ..Options::default() };
    // Four exact points are enough: the plane's homography gives the focal length too.
    let mut rng = Lcg(4);
    let data = pairs(&cam, &wall(&mut rng, 4), &mut rng, 0.0);
    let s = calibrate(&data, SIZE, None, &opts).unwrap();
    assert_eq!(s.start, Start::Plane);
    assert!(s.rms < 1e-6, "{}", s.rms);
    assert!(
        (s.camera.intrinsics.focal[0] - cam.intrinsics.focal[0]).abs() < 1e-3,
        "{:?}",
        s.camera.intrinsics
    );
    assert!(dist3(s.camera.position(), cam.position()) < 1e-6);
    // With noise on 15 points.
    let data = pairs(&cam, &wall(&mut rng, 15), &mut rng, 0.5);
    let s = calibrate(&data, SIZE, None, &opts).unwrap();
    // A plane pins the focal length down less well than an object: 2.5 % off, 9 cm away.
    assert!(s.rms <= cam.rms_error(&data));
    assert!((s.camera.intrinsics.focal[0] / cam.intrinsics.focal[0] - 1.0).abs() < 0.05);
    assert!(dist3(s.camera.position(), cam.position()) < 0.15, "{:?}", s.camera.position());
    // Without the fixed principal point a plane cannot calibrate.
    let e = calibrate(&data, SIZE, None, &Options::default()).unwrap_err();
    assert!(e.to_string().contains("principal point"), "{e}");
}

#[test]
fn a_wall_seen_head_on_needs_a_focal_length() {
    // Square to the wall, the plane cannot tell focal length from distance.
    let pose = Pose::look_at([0.0, 0.0, -3.0], [0.0, 0.0, 0.0], [0.0, -1.0, 0.0]).unwrap();
    let cam = Camera { intrinsics: Intrinsics::from_throw_ratio(SIZE, 1.2), pose };
    let mut rng = Lcg(5);
    let data = pairs(&cam, &wall(&mut rng, 8), &mut rng, 0.0);
    let opts = Options { fix_principal_point: true, ..Options::default() };
    assert!(calibrate(&data, SIZE, None, &opts).is_err());
    // With the lens known (an initial camera, e.g. from the throw ratio) the pose comes back.
    let guess = Camera { intrinsics: cam.intrinsics, pose: Pose::default() };
    let opts = Options { fix_focal: true, fix_principal_point: true, ..Options::default() };
    let s = calibrate(&data, SIZE, Some(&guess), &opts).unwrap();
    assert!(s.rms < 1e-6, "{}", s.rms);
    assert!(dist3(s.camera.position(), cam.position()) < 1e-6);
}

#[test]
fn a_thin_object_with_a_free_principal_point() {
    // A relief 4 cm deep on a 2.4 m wall: the DLT is shaky there, the plane's start helps.
    let cam = truth();
    let mut rng = Lcg(6);
    let world: Vec<[f64; 3]> =
        (0..25).map(|_| [rng.range(-1.2, 1.2), rng.range(-0.7, 0.7), rng.range(0.0, 0.04)]).collect();
    let data = pairs(&cam, &world, &mut rng, 0.3);
    let s = calibrate(&data, SIZE, None, &Options::default()).unwrap();
    assert!(s.rms <= cam.rms_error(&data));
}

#[test]
fn known_lens_needs_only_the_pose() {
    let cam = truth();
    let mut rng = Lcg(7);
    let opts = Options { fix_focal: true, fix_principal_point: true, ..Options::default() };
    // A guess from the wrong side of the room: the DLT's start wins.
    let bad = Pose::look_at([3.0, 0.0, 4.0], [0.0, 0.0, 0.0], [0.0, -1.0, 0.0]).unwrap();
    let guess = Camera { intrinsics: cam.intrinsics, pose: bad };
    let data = pairs(&cam, &object(&mut rng, 6), &mut rng, 0.0);
    let s = calibrate(&data, SIZE, Some(&guess), &opts).unwrap();
    assert!(s.rms < 1e-6, "{}", s.rms);
    assert!(dist3(s.camera.position(), cam.position()) < 1e-7);
    assert_eq!(s.camera.intrinsics.focal, cam.intrinsics.focal);
    // Four points not on a plane and a rough guess (last calibration, projector nudged).
    let near = Pose {
        rotation: [cam.pose.rotation[0] + 0.03, cam.pose.rotation[1] - 0.02, cam.pose.rotation[2]],
        translation: [cam.pose.translation[0] + 0.1, cam.pose.translation[1], cam.pose.translation[2] - 0.2],
    };
    let guess = Camera { intrinsics: cam.intrinsics, pose: near };
    let data = pairs(&cam, &object(&mut rng, 4), &mut rng, 0.0);
    let s = calibrate(&data, SIZE, Some(&guess), &opts).unwrap();
    assert_eq!(s.start, Start::Initial);
    assert!(s.rms < 1e-6, "{}", s.rms);
    assert!(dist3(s.camera.position(), cam.position()) < 1e-6);
    // The same four points without a guess are not enough.
    assert!(calibrate(&data, SIZE, None, &Options::default()).is_err());
}

#[test]
fn radial_distortion_and_non_square_pixels() {
    // A camera rather than a projector: barrel distortion and slightly non-square pixels.
    let pose = Pose::look_at([0.3, -0.4, -2.0], [0.0, 0.1, 0.0], [0.0, -1.0, 0.0]).unwrap();
    let mut cam = Camera { pose, ..truth() };
    cam.intrinsics.distortion = [-0.12, 0.05];
    cam.intrinsics.focal = [1500.0, 1530.0];
    cam.intrinsics.principal = [950.0, 560.0];
    let mut rng = Lcg(8);
    let data = pairs(&cam, &object(&mut rng, 80), &mut rng, 0.3);
    let opts = Options { square_pixels: false, distortion: true, ..Options::default() };
    let s = calibrate(&data, SIZE, None, &opts).unwrap();
    let k = &s.camera.intrinsics;
    assert!(s.rms <= cam.rms_error(&data));
    assert!((k.focal[0] - 1500.0).abs() < 15.0 && (k.focal[1] - 1530.0).abs() < 15.0, "{:?}", k.focal);
    assert!((k.distortion[0] + 0.12).abs() < 0.02, "{:?}", k.distortion);
    // Measured: focal lengths within 0.3 px, k1 within 0.004, 0.3 px RMS on the object.
    assert!(agreement_rms(&s.camera, &cam) < 0.5 && agreement(&s.camera, &cam) < 2.0);
    // Without distortion in the model the fit is visibly worse.
    let s2 = calibrate(&data, SIZE, None, &Options { square_pixels: false, ..Options::default() }).unwrap();
    assert!(s2.rms > 2.0 * s.rms, "{} vs {}", s2.rms, s.rms);
}

#[test]
fn degenerate_input_is_refused() {
    let cam = truth();
    let mut rng = Lcg(9);
    let good = pairs(&cam, &object(&mut rng, 8), &mut rng, 0.0);
    let opts = Options::default();
    let fails = |data: &[([f64; 3], [f64; 2])], opts: &Options| calibrate(data, SIZE, None, opts).is_err();

    assert!(fails(&good[..3], &opts));
    assert!(fails(&[], &opts));
    for bad in [f64::NAN, f64::INFINITY, 1e12] {
        let mut d = good.clone();
        d[2].0[1] = bad;
        assert!(fails(&d, &opts), "{bad}");
        let mut d = good.clone();
        d[3].1[0] = bad;
        assert!(fails(&d, &opts), "{bad}");
    }
    // All world points in one place, and on one line.
    let same: Vec<_> = good.iter().map(|(_, q)| ([0.2, 0.1, 0.0], *q)).collect();
    assert!(fails(&same, &opts));
    let line: Vec<_> = (0..8).map(|i| [0.1 * i as f64, 0.05 * i as f64, -0.02 * i as f64]).collect();
    assert!(fails(&pairs(&cam, &line, &mut rng, 0.0), &opts));
    // Five points not on a plane, no guess.
    assert!(fails(&good[..5], &opts));
    // All picture points in one place.
    let blob: Vec<_> = good.iter().map(|(w, _)| (*w, [500.0, 500.0])).collect();
    assert!(fails(&blob, &opts));
    // A mirrored picture (rear-projection mode): the world seen through a mirror.
    let mirrored: Vec<_> = good.iter().map(|(w, q)| (*w, [1920.0 - q[0], q[1]])).collect();
    let e = calibrate(&mirrored, SIZE, None, &opts);
    assert!(e.is_err() || e.as_ref().unwrap().rms > 10.0, "{e:?}");
    // Bad sizes, options and initial cameras.
    assert!(calibrate(&good, [0, 1080], None, &opts).is_err());
    assert!(fails(&good, &Options { max_iterations: 0, ..opts }));
    assert!(fails(&good, &Options { fix_focal: true, ..opts }));
    let mut other = cam;
    other.intrinsics.size = [1280, 720];
    assert!(calibrate(&good, SIZE, Some(&other), &opts).is_err());
    let mut nan = cam;
    nan.pose.rotation[0] = f64::NAN;
    assert!(calibrate(&good, SIZE, Some(&nan), &opts).is_err());
    let mut zero = cam;
    zero.intrinsics.focal = [0.0, 0.0];
    assert!(calibrate(&good, SIZE, Some(&zero), &opts).is_err());
    // Four points on a wall give 8 equations, too few for pose, focal length and distortion.
    let w = pairs(&wall_camera(), &wall(&mut rng, 4), &mut rng, 0.0);
    let e = calibrate(&w, SIZE, None, &Options { distortion: true, fix_principal_point: true, ..opts })
        .unwrap_err();
    assert!(e.to_string().contains("free parameters"), "{e}");
}

#[test]
fn garbage_never_panics_and_ends() {
    // Random pairs with no camera behind them: an answer or an error, never a panic or a hang.
    for seed in 0..60 {
        let mut rng = Lcg(1000 + seed);
        let n = 4 + (rng.next() * 12.0) as usize;
        let data: Vec<_> = (0..n)
            .map(|_| {
                let w =
                    [rng.range(-1.0, 1.0), rng.range(-1.0, 1.0), rng.range(-1.0, 1.0) * (seed % 3) as f64];
                (w, [rng.range(-500.0, 2500.0), rng.range(-500.0, 1500.0)])
            })
            .collect();
        let opts =
            Options { fix_principal_point: seed % 2 == 0, distortion: seed % 5 == 0, ..Options::default() };
        if let Ok(s) = calibrate(&data, SIZE, None, &opts) {
            assert!(s.rms.is_finite() && s.iterations <= opts.max_iterations);
        }
    }
}

#[test]
fn the_iteration_limit_holds() {
    let cam = truth();
    let mut rng = Lcg(10);
    let data = pairs(&cam, &object(&mut rng, 20), &mut rng, 0.5);
    let guess = Camera {
        intrinsics: cam.intrinsics,
        pose: Pose::look_at([0.0, 0.0, -9.0], [1.0, 1.0, 0.0], [0.0, -1.0, 0.0]).unwrap(),
    };
    let s =
        calibrate(&data, SIZE, Some(&guess), &Options { max_iterations: 1, ..Options::default() }).unwrap();
    assert_eq!(s.iterations, 1);
}

#[test]
fn camera_round_trips_through_json() {
    let cam = truth();
    let json = serde_json::to_string(&cam).unwrap();
    let back: Camera = serde_json::from_str(&json).unwrap();
    assert_eq!(back.intrinsics, cam.intrinsics);
    assert!(dist3(back.pose.translation, cam.pose.translation) < 1e-15);
    // Distortion may be left out.
    let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
    v["intrinsics"].as_object_mut().unwrap().remove("distortion");
    let back: Camera = serde_json::from_value(v).unwrap();
    assert_eq!(back.intrinsics.distortion, [0.0, 0.0]);
}
