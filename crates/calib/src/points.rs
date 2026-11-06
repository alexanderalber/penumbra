//! A list of point pairs kept with a camera, and what can be said about the fit they give: which
//! point looks wrong, how far the picture may be off on each part of the object, and how well a
//! camera's place is known.
//!
//! The thresholds come from simulating a thin object of six plates, 535 mm long, with click noise
//! of 1 to 3 pixels, a 1080p picture about 1 m wide, and 6 to 40 points.
//!
//! - The expected deviation matches the real error to about a factor of 1 while it stays below
//!   8 px. Above that the fit is close to degenerate and the real error can be many times larger.
//! - Leaving a point out and dividing its error by what is expected there names one bad click far
//!   better than its error in pixels: points at the rim read large errors for no fault of their
//!   own. Below 10 points this still raises false alarms in more than one fit out of ten.

use std::collections::BTreeMap;

use penumbra_core::Error;
use serde::{Deserialize, Serialize};

use crate::camera::{Camera, Intrinsics, Pose};
use crate::solve::{Options, Solution, calibrate, expected_deviation, leave_one_out};

/// A world point and the picture point it belongs to.
type Pair = ([f64; 3], [f64; 2]);

/// What the picture points are clicked on, which decides what stays free in the fit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PictureKind {
    /// A projector's own picture: the principal point stays free, as lens shift puts it anywhere.
    #[default]
    Projector,
    /// A still from a camera: the principal point sits at the picture's centre, as cameras have no
    /// lens shift. A cropped picture breaks this; turn `fix_principal_point` off for one.
    Picture,
}

impl PictureKind {
    /// The options a fit of this kind starts from.
    pub fn options(self) -> Options {
        match self {
            PictureKind::Projector => Options::default(),
            PictureKind::Picture => Options { fix_principal_point: true, ..Options::default() },
        }
    }
}

/// A point of the model and the place in the picture it belongs to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Point {
    /// Stays with the point when others come and go.
    pub id: u32,
    /// In the model's coordinates (world units).
    pub world: [f64; 3],
    /// Pixels of the picture, origin at its top left corner.
    pub picture: [f64; 2],
    /// Which part of the model it lies on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Kept in the list but left out of the fit.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub off: bool,
}

/// The point pairs of one camera, as stored with it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PointSet {
    /// The picture the pixels belong to.
    pub size: [u32; 2],
    #[serde(default)]
    pub kind: PictureKind,
    pub options: Options,
    /// A known focal length in pixels, held fixed. Never needed: the fit finds one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focal: Option<f64>,
    #[serde(default)]
    pub points: Vec<Point>,
}

/// Noise of a click the warnings assume at least, per axis in pixels. With few points the
/// residuals can read far less by luck, and the warning would stay silent.
pub const MIN_CLICK_NOISE: f64 = 1.0;
/// A point's error over what is expected there, above which it counts as a suspect.
pub const SUSPECT_RATIO: f64 = 3.0;
/// Below this many points in the fit nobody is named a suspect: the fit follows a bad point so
/// closely, and false alarms come so often, that the name would mislead.
pub const SUSPECT_MIN_POINTS: usize = 10;
/// Below this many points in the fit the result is called weak at best.
pub const WEAK_BELOW_POINTS: usize = 8;

/// How much a fit can be trusted, by the largest expected deviation on the object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    /// Under 2 px.
    Good,
    /// 2 to 4 px.
    Fair,
    /// 4 to 8 px, or too few points.
    Weak,
    /// 8 px or more: the real error may be many times that.
    Unusable,
}

impl Quality {
    pub fn of(deviation: f64, points: usize) -> Self {
        let by_size = if !deviation.is_finite() || deviation >= 8.0 {
            Quality::Unusable
        } else if deviation >= 4.0 {
            Quality::Weak
        } else if deviation >= 2.0 {
            Quality::Fair
        } else {
            Quality::Good
        };
        if points < WEAK_BELOW_POINTS { by_size.max(Quality::Weak) } else { by_size }
    }
}

/// One point's part in the fit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PointReport {
    pub id: u32,
    /// Distance between its picture point and the fit, in pixels.
    pub error: f64,
    /// The same with the point left out of the fit; `None` when the others do not determine the
    /// camera. This is the honest number: the fit bends towards a point it holds.
    pub left_out: Option<f64>,
    /// What `left_out` may be by chance, one standard deviation in pixels.
    pub expected: Option<f64>,
    /// `left_out / expected`. Compare points by this, not by pixels.
    pub ratio: Option<f64>,
}

/// Where the camera is, in the model's coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PoseReport {
    pub position: [f64; 3],
    /// The direction it looks in.
    pub forward: [f64; 3],
    /// The picture's up in the world.
    pub up: [f64; 3],
    /// Distance from the camera to the middle of the points.
    pub distance: f64,
    /// One standard deviation of the position along the view axis (world units): how well that
    /// distance is known. `None` without redundancy.
    pub distance_spread: Option<f64>,
    /// One standard deviation of the position along the world axes.
    pub position_spread: Option<[f64; 3]>,
    /// One standard deviation of the view direction, in degrees.
    pub direction_spread: Option<f64>,
    /// The focal length in pixels and as a field of view across the picture's width, in degrees.
    pub focal: f64,
    pub fov: f64,
}

/// The expected deviation on one part of the object.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GroupDeviation {
    pub group: String,
    /// Root mean square over the group's points, in pixels.
    pub rms: f64,
    /// Largest in the group, in pixels.
    pub max: f64,
    pub points: usize,
}

/// What a fit of a [`PointSet`] came to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub solution: Solution,
    pub pose: PoseReport,
    pub points: Vec<PointReport>,
    /// The ids of points that look wrong, worst first.
    pub suspects: Vec<u32>,
    /// The noise of a click used for the expected deviations, per axis in pixels.
    pub noise: f64,
    /// Sentences for the user: what is weak and what to do about it.
    pub warnings: Vec<String>,
}

impl PointSet {
    pub fn new(size: [u32; 2], kind: PictureKind) -> Self {
        Self { size, kind, options: kind.options(), focal: None, points: Vec::new() }
    }

    /// The next free id.
    pub fn next_id(&self) -> u32 {
        self.points.iter().map(|p| p.id.saturating_add(1)).max().unwrap_or(1)
    }

    /// The pairs that take part in the fit, with their ids.
    fn active(&self) -> (Vec<Pair>, Vec<u32>) {
        self.points.iter().filter(|p| !p.off).map(|p| ((p.world, p.picture), p.id)).unzip()
    }

    fn options_and_initial(&self) -> Result<(Options, Option<Camera>), Error> {
        let [w, h] = self.size.map(f64::from);
        match self.focal {
            None => Ok((self.options, None)),
            Some(f) if f.is_finite() && f > 0.0 => {
                let lens = Intrinsics {
                    size: self.size,
                    focal: [f, f],
                    principal: [w / 2.0, h / 2.0],
                    distortion: [0.0; 2],
                };
                let options = Options { fix_focal: true, ..self.options };
                Ok((options, Some(Camera { intrinsics: lens, pose: Pose::default() })))
            }
            Some(_) => Err(Error::new("a known focal length must be a positive number")),
        }
    }

    /// Fits the camera to the points and judges the result.
    pub fn fit(&self) -> Result<Report, Error> {
        let (pairs, ids) = self.active();
        let (options, initial) = self.options_and_initial()?;
        let solution = calibrate(&pairs, self.size, initial.as_ref(), &options)?;
        let camera = solution.camera;
        let spread = solution.spread;
        let noise = spread.map_or(MIN_CLICK_NOISE, |s| s.sigma.max(MIN_CLICK_NOISE));

        let left = leave_one_out(&pairs, &camera, &options)?;
        let points: Vec<PointReport> = ids
            .iter()
            .zip(&solution.errors)
            .zip(&left)
            .map(|((id, error), l)| PointReport {
                id: *id,
                error: *error,
                left_out: l.map(|v| v.error),
                expected: l.map(|v| v.expected),
                ratio: l.map(|v| v.error / v.expected),
            })
            .collect();
        let mut named: Vec<(u32, f64)> = points
            .iter()
            .filter_map(|p| Some((p.id, p.ratio?)))
            .filter(|(_, r)| *r > SUSPECT_RATIO && pairs.len() >= SUSPECT_MIN_POINTS)
            .collect();
        named.sort_by(|a, b| b.1.total_cmp(&a.1));
        let suspects: Vec<u32> = named.iter().map(|n| n.0).collect();

        let pose = pose_report(&solution, &pairs);
        let mut warnings = Vec::new();
        if !solution.converged {
            warnings.push("The fit stopped at its iteration limit before it settled.".to_string());
        }
        if pairs.len() < WEAK_BELOW_POINTS {
            warnings.push(format!(
                "{} points: the picture can still be far off away from them. 8 or more on at least 2 plates are safer.",
                pairs.len()
            ));
        }
        if pairs.len() < SUSPECT_MIN_POINTS {
            warnings.push(format!(
                "A wrong point cannot be told reliably below {SUSPECT_MIN_POINTS} points. The ratios are shown, but nobody is named."
            ));
        }
        if let Some(first) = suspects.first() {
            warnings.push(format!("Point {first} looks wrong: check it first, a good fit depends on it."));
        }
        if self.kind == PictureKind::Picture
            && let Some(sd) = pose.distance_spread
            && sd > 0.05 * pose.distance
        {
            warnings.push(format!(
                "The distance to the object is known only to about {:.0} % ({:.0} mm in metres). Without a known focal length, a more oblique view of the object helps most.",
                100.0 * sd / pose.distance,
                1000.0 * sd
            ));
        }
        Ok(Report { solution, pose, points, suspects, noise, warnings })
    }

    /// The expected deviation of `camera` on each group of points, in the order of the names.
    /// `noise` is the noise of a click per axis in pixels; without it the residuals tell, but not
    /// less than [`MIN_CLICK_NOISE`].
    pub fn deviation(
        &self,
        camera: &Camera,
        groups: &BTreeMap<String, Vec<[f64; 3]>>,
        noise: Option<f64>,
    ) -> Result<(Vec<GroupDeviation>, Quality), Error> {
        let (pairs, _) = self.active();
        let (options, _) = self.options_and_initial()?;
        let noise = match noise {
            Some(n) => n,
            None => {
                let errors = camera.errors(&pairs);
                let dof = (2 * pairs.len()).saturating_sub(free_count(&options)).max(1);
                let sum: f64 = errors.iter().map(|e| e * e).sum();
                (sum / dof as f64).sqrt().max(MIN_CLICK_NOISE)
            }
        };
        let mut out = Vec::with_capacity(groups.len());
        let mut worst = 0.0f64;
        for (group, points) in groups {
            let e = expected_deviation(&pairs, camera, &options, Some(noise), points)?;
            let values: Vec<f64> = e.iter().map(|v| v.unwrap_or(f64::INFINITY)).collect();
            let n = values.len().max(1) as f64;
            let rms = (values.iter().map(|v| v * v).sum::<f64>() / n).sqrt();
            let max = values.iter().copied().fold(0.0, f64::max);
            worst = worst.max(rms);
            out.push(GroupDeviation { group: group.clone(), rms, max, points: values.len() });
        }
        Ok((out, Quality::of(worst, pairs.len())))
    }
}

fn free_count(o: &Options) -> usize {
    let focal = match (o.fix_focal, o.square_pixels) {
        (true, _) => 0,
        (false, true) => 1,
        (false, false) => 2,
    };
    6 + focal + if o.fix_principal_point { 0 } else { 2 } + if o.distortion { 2 } else { 0 }
}

fn pose_report(s: &Solution, pairs: &[Pair]) -> PoseReport {
    let camera = &s.camera;
    let position = camera.position();
    let [_, r1, r2] = camera.pose.rotation_matrix();
    let n = pairs.len().max(1) as f64;
    let middle = pairs.iter().fold([0.0; 3], |a, (w, _)| [a[0] + w[0] / n, a[1] + w[1] / n, a[2] + w[2] / n]);
    let distance = middle.iter().zip(&position).map(|(a, b)| (a - b).powi(2)).sum::<f64>().sqrt();
    let focal = camera.intrinsics.focal[0];
    let width = f64::from(camera.intrinsics.size[0]);
    PoseReport {
        position,
        forward: r2,
        // Picture y runs down.
        up: [-r1[0], -r1[1], -r1[2]],
        distance,
        distance_spread: s.spread.map(|v| v.distance),
        position_spread: s.spread.map(|v| v.position),
        direction_spread: s.spread.map(|v| v.direction),
        focal,
        fov: 2.0 * (width / 2.0 / focal).atan().to_degrees(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A thin object: corners of two plates 30 mm apart, 500 x 300 mm, seen from the side.
    fn truth(kind: PictureKind) -> Camera {
        let pose = Pose::look_at([0.9, -1.6, 0.1], [0.0, 0.0, 0.0], [0.0, 0.0, 1.0]).unwrap();
        let mut intrinsics = Intrinsics::from_throw_ratio([1920, 1080], 1.4);
        if kind == PictureKind::Projector {
            intrinsics.principal = [960.0, 1000.0];
        }
        Camera { intrinsics, pose }
    }

    fn corners() -> Vec<[f64; 3]> {
        let mut v = Vec::new();
        for (i, y) in [-0.03, -0.02, -0.01, 0.0].into_iter().enumerate() {
            for k in 0..6 {
                let a = (k as f64 + 0.3 * i as f64) * std::f64::consts::TAU / 6.0;
                v.push([0.14 * a.cos(), y, 0.25 * a.sin()]);
            }
        }
        v
    }

    /// A small repeating noise of about `size` pixels.
    fn wiggle(i: usize, size: f64) -> [f64; 2] {
        let a = (i as f64 * 12.9898).sin() * 43758.5453;
        let b = (i as f64 * 78.233).sin() * 12543.123;
        [(a.fract()) * 2.0 * size, (b.fract()) * 2.0 * size]
    }

    fn set(kind: PictureKind, noise: f64) -> PointSet {
        let cam = truth(kind);
        let mut s = PointSet::new([1920, 1080], kind);
        for (i, w) in corners().into_iter().enumerate() {
            let q = cam.project(w).unwrap();
            let d = wiggle(i, noise);
            s.points.push(Point {
                id: s.next_id(),
                world: w,
                picture: [q[0] + d[0], q[1] + d[1]],
                group: Some(format!("plate{}", i / 6)),
                off: false,
            });
        }
        s
    }

    #[test]
    fn a_fit_of_clean_points_finds_the_camera_and_names_nobody() {
        let s = set(PictureKind::Projector, 0.5);
        let r = s.fit().unwrap();
        let cam = truth(PictureKind::Projector);
        assert!(r.solution.rms < 1.0, "{}", r.solution.rms);
        assert_eq!(r.points.len(), 24);
        assert!(r.suspects.is_empty(), "{:?}", r.suspects);
        // On the object, fit and truth agree whatever focal length and distance came out.
        for w in corners() {
            let (a, b) = (r.solution.camera.project(w).unwrap(), cam.project(w).unwrap());
            assert!((a[0] - b[0]).hypot(a[1] - b[1]) < 2.0);
        }
    }

    #[test]
    fn a_bad_click_is_named() {
        let mut s = set(PictureKind::Projector, 0.5);
        s.points[7].picture[0] += 25.0;
        let r = s.fit().unwrap();
        assert_eq!(r.suspects.first(), Some(&8), "{:?}", r.points);
        assert!(r.warnings.iter().any(|w| w.contains("Point 8")), "{:?}", r.warnings);
        // Turned off, the fit is clean again and the list keeps the point.
        s.points[7].off = true;
        let r = s.fit().unwrap();
        assert!(r.suspects.is_empty(), "{:?}", r.suspects);
        assert_eq!(r.points.len(), 23);
        assert_eq!(s.points.len(), 24);
    }

    #[test]
    fn few_points_name_nobody_and_say_so() {
        let mut s = set(PictureKind::Projector, 0.5);
        s.points.truncate(9);
        s.points[3].picture[0] += 40.0;
        let r = s.fit().unwrap();
        assert!(r.suspects.is_empty());
        assert!(r.warnings.iter().any(|w| w.contains("cannot be told")), "{:?}", r.warnings);
        s.points.truncate(6);
        let w = s.fit().map(|r| r.warnings).unwrap_or_default();
        assert!(w.is_empty() || w.iter().any(|t| t.contains("6 points")), "{w:?}");
    }

    #[test]
    fn the_deviation_grows_with_noise_and_falls_with_points() {
        let s = set(PictureKind::Projector, 0.5);
        let r = s.fit().unwrap();
        let groups: BTreeMap<String, Vec<[f64; 3]>> = BTreeMap::from([
            ("all".to_string(), corners()),
            ("rim".to_string(), vec![[0.14, -0.03, 0.0], [-0.14, 0.0, 0.0]]),
        ]);
        let cam = &r.solution.camera;
        let (one, q1) = s.deviation(cam, &groups, Some(1.0)).unwrap();
        let (two, _) = s.deviation(cam, &groups, Some(2.0)).unwrap();
        assert_eq!(one.len(), 2);
        assert!((two[0].rms / one[0].rms - 2.0).abs() < 1e-6);
        assert!(one[0].rms > 0.0 && one[0].rms < 2.0, "{one:?}");
        assert_eq!(q1, Quality::Good);
        let mut few = s.clone();
        few.points.truncate(8);
        let solved = few.fit().unwrap();
        let (three, _) = few.deviation(&solved.solution.camera, &groups, Some(1.0)).unwrap();
        assert!(three[0].rms > one[0].rms, "{three:?} {one:?}");
    }

    #[test]
    fn a_picture_gives_a_pose_and_how_well_it_is_known() {
        let s = set(PictureKind::Picture, 0.5);
        assert!(s.options.fix_principal_point);
        let r = s.fit().unwrap();
        let cam = truth(PictureKind::Picture);
        let (a, b) = (r.pose.position, cam.position());
        let off = (0..3).map(|k| (a[k] - b[k]).powi(2)).sum::<f64>().sqrt();
        let sd = r.pose.distance_spread.unwrap();
        assert!(sd > 0.0 && off < 5.0 * sd.max(0.01), "off {off}, sd {sd}");
        // Forward and up are unit vectors at right angles, up points up.
        let dot: f64 = (0..3).map(|k| r.pose.forward[k] * r.pose.up[k]).sum();
        assert!(dot.abs() < 1e-9 && r.pose.up[2] > 0.5, "{:?}", r.pose);
        // A known focal length pins the distance down.
        let mut known = s.clone();
        known.focal = Some(cam.intrinsics.focal[0]);
        let k = known.fit().unwrap();
        assert!(k.pose.distance_spread.unwrap() < sd, "{:?} {sd}", k.pose.distance_spread);
        assert!((k.pose.focal - cam.intrinsics.focal[0]).abs() < 1e-9);
    }

    #[test]
    fn the_stored_text_comes_back_and_bad_text_is_an_error() {
        let s = set(PictureKind::Projector, 0.5);
        let text = serde_json::to_string(&s).unwrap();
        let back = serde_json::from_str::<PointSet>(&text).unwrap();
        assert_eq!(back.points.len(), s.points.len());
        assert_eq!(back.points[7].id, s.points[7].id);
        assert!((back.points[7].picture[0] - s.points[7].picture[0]).abs() < 1e-9);
        assert_eq!(back.points[7].group, s.points[7].group);
        let mut bad = s.clone();
        bad.points.truncate(2);
        assert!(bad.fit().is_err());
        bad = s;
        bad.focal = Some(-1.0);
        assert!(bad.fit().is_err());
    }
}
