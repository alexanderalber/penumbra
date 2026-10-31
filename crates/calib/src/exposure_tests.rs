use super::*;

const W: usize = 64;
const H: usize = 64;

/// A picture whose every pixel is `f(x, y)` (encoded grey, decoded to linear as the camera
/// reader does).
fn picture(f: impl Fn(usize, usize) -> f64) -> Picture {
    let mut rgb = Vec::with_capacity(W * H);
    for y in 0..H {
        for x in 0..W {
            let v = decode(f(x, y)) as f32;
            rgb.push([v; 3]);
        }
    }
    Picture { width: W, height: H, rgb }
}

fn all() -> Vec<bool> {
    vec![true; W * H]
}

fn black() -> Picture {
    picture(|_, _| 0.05)
}

#[test]
fn a_good_white_is_ok_and_needs_nothing() {
    let white = picture(|x, _| 0.70 + 0.1 * x as f64 / W as f64);
    let r = read(&[white.clone(), white], &black(), &all()).unwrap();
    assert_eq!(r.verdict(), Verdict::Ok, "{r:?}");
    assert!((r.peak - 0.80).abs() < 0.01, "{r:?}");
    assert!((r.black - 0.05).abs() < 0.01);
    assert_eq!(r.clipped, 0.0);
    assert!(r.advice().is_none(), "{:?}", r.advice());
}

#[test]
fn a_clipped_corner_refuses_and_says_darker() {
    // A tenth of the spot at the top of the range.
    let white = picture(|x, _| if x < W / 10 + 1 { 1.0 } else { 0.8 });
    let r = read(&[white], &black(), &all()).unwrap();
    assert_eq!(r.verdict(), Verdict::Clipped);
    assert!(r.clipped > 0.09 && r.clipped < 0.13, "{r:?}");
    assert_eq!(r.clipped_channels, [true; 3]);
    assert!(r.stops() <= -1.0);
    assert!(r.advice().unwrap().contains("lower the exposure"));
}

#[test]
fn clipping_in_one_frame_counts() {
    let good = picture(|_, _| 0.8);
    let hot = picture(|x, y| if x < 8 && y < 8 { 1.0 } else { 0.8 });
    let r = read(&[good.clone(), hot, good], &black(), &all()).unwrap();
    assert_eq!(r.verdict(), Verdict::Clipped, "{r:?}");
}

#[test]
fn only_the_mask_is_read() {
    let white = picture(|x, _| if x < W / 2 { 1.0 } else { 0.78 });
    let mask: Vec<bool> = (0..W * H).map(|i| i % W >= W / 2).collect();
    let r = read(&[white], &black(), &mask).unwrap();
    assert_eq!(r.verdict(), Verdict::Ok, "{r:?}");
    assert_eq!(r.pixels, W * H / 2);
}

#[test]
fn a_dim_white_asks_for_more() {
    let white = picture(|_, _| 0.3);
    let r = read(&[white], &black(), &all()).unwrap();
    assert_eq!(r.verdict(), Verdict::Dark);
    assert!(r.stops() > 1.0, "{}", r.stops());
    assert!(r.advice().unwrap().starts_with("raise"));
}

#[test]
fn bands_from_a_short_shutter_are_unsteady() {
    // A dark band that moves down from frame to frame, like a rolling shutter catching part
    // of a projector frame; the mean over the picture stays the same.
    let frames: Vec<Picture> =
        (0..4).map(|k| picture(move |_, y| if (y / 8) % 4 == k { 0.70 } else { 0.78 })).collect();
    let r = read(&frames, &black(), &all()).unwrap();
    assert!(r.banding > UNSTEADY, "{r:?}");
    assert_eq!(r.verdict(), Verdict::Unsteady);
    assert!(r.flicker < 0.01, "{r:?}");
    assert!(r.advice().unwrap().contains("row to row"));
}

#[test]
fn a_smooth_falloff_is_not_banding() {
    // The projector's own vignetting: brighter in the middle, over the whole height.
    let white = picture(|_, y| {
        let d = (y as f64 - H as f64 / 2.0) / H as f64;
        0.8 - 0.4 * d * d
    });
    let r = read(&[white.clone(), white], &black(), &all()).unwrap();
    assert!(r.banding < 1e-9, "{r:?}");
    assert_eq!(r.verdict(), Verdict::Ok);
}

#[test]
fn frames_that_differ_are_unsteady() {
    let a = picture(|_, _| 0.75);
    let b = picture(|_, _| 0.80);
    let r = read(&[a, b], &black(), &all()).unwrap();
    assert!(r.flicker > 0.1, "{r:?}");
    assert_eq!(r.verdict(), Verdict::Unsteady);
    assert!(r.advice().unwrap().contains("frame to frame"));
}

#[test]
fn mismatched_or_empty_input_reads_nothing() {
    let white = picture(|_, _| 0.8);
    assert!(read(&[], &black(), &all()).is_none());
    assert!(read(std::slice::from_ref(&white), &black(), &vec![false; W * H]).is_none());
    assert!(read(std::slice::from_ref(&white), &black(), &[true; 3]).is_none());
    let small = Picture { width: 2, height: 2, rgb: vec![[0.5; 3]; 4] };
    assert!(read(&[white], &small, &all()).is_none());
}
