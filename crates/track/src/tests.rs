use crate::{Blob, Detect, Image, Mask, Settings, Tracker, components};

const GREY: [f32; 3] = [0.2, 0.2, 0.2];
const RED: [f32; 3] = [0.8, 0.1, 0.1];

fn room() -> Image {
    Image::filled(160, 90, GREY)
}

/// A filled disc of `color` with centre `(cx, cy)` in pixels.
fn disc(img: &mut Image, cx: f32, cy: f32, r: f32, color: [f32; 3]) {
    for y in 0..img.height {
        for x in 0..img.width {
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            if dx * dx + dy * dy <= r * r {
                img.set(x, y, color);
            }
        }
    }
}

fn mask_from(rows: &[&str]) -> Mask {
    let width = rows.first().map_or(0, |r| r.len());
    let data = rows.iter().flat_map(|r| r.bytes().map(|b| u8::from(b == b'#'))).collect();
    Mask { width, height: rows.len(), data }
}

fn rows_of(m: &Mask) -> Vec<String> {
    m.data.chunks(m.width).map(|r| r.iter().map(|v| if *v != 0 { '#' } else { '.' }).collect()).collect()
}

#[test]
fn opening_removes_specks_and_keeps_squares() {
    let mut m = mask_from(&["#.......", "........", "..###...", "..###...", "..###...", "........"]);
    m.open(1);
    assert_eq!(rows_of(&m), ["........", "........", "..###...", "..###...", "..###...", "........"]);
}

#[test]
fn closing_fills_a_hole() {
    let mut m = mask_from(&[".....", ".###.", ".#.#.", ".###.", "....."]);
    m.close(1);
    assert!(m.get(2, 2));
    assert!(!m.get(0, 0), "closing does not grow the outline: {:?}", rows_of(&m));
}

#[test]
fn erosion_keeps_regions_at_the_border() {
    let mut m = mask_from(&["###.....", "###.....", "###....."]);
    m.erode(1);
    assert_eq!(rows_of(&m), ["##......", "##......", "##......"]);
}

#[test]
fn components_are_eight_connected_and_largest_first() {
    let m = mask_from(&["#.....##", ".#....##", "..#.....", "........", "....#..."]);
    let r = components(&m, &[]);
    assert_eq!(r.iter().map(|r| r.area).collect::<Vec<_>>(), [4, 3, 1]);
    assert_eq!(r[1].min, [0, 0]);
    assert_eq!(r[1].max, [2, 2]);
    assert_eq!(r[0].centre, [7.0, 1.0]);
}

#[test]
fn a_u_shape_is_one_region() {
    // Two arms that only meet at the bottom: the labels from the top merge late.
    let m = mask_from(&["#...#", "#...#", "#...#", "#####"]);
    let r = components(&m, &[]);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].area, 11);
}

fn settings() -> Settings {
    Settings { adapt: 0.0, ..Settings::default() }
}

#[test]
fn the_first_frame_is_the_background() {
    let mut t = Tracker::new();
    let mut img = room();
    disc(&mut img, 40.0, 40.0, 8.0, RED);
    assert!(t.update(&img, 1.0 / 60.0, &settings()).is_empty());
    assert!(
        t.update(&img, 1.0 / 60.0, &settings()).is_empty(),
        "what was there from the start is background"
    );
}

#[test]
fn a_disc_is_found_where_it_is() {
    let mut t = Tracker::new();
    t.update(&room(), 0.016, &settings());
    let mut img = room();
    disc(&mut img, 40.0, 30.0, 8.0, RED);
    let blobs = t.update(&img, 0.016, &settings());
    assert_eq!(blobs.len(), 1);
    let b = &blobs[0];
    assert_eq!(b.id, 1);
    assert!((b.x - 40.0 / 160.0).abs() < 0.005, "{b:?}");
    assert!((b.y - 30.0 / 90.0).abs() < 0.005, "{b:?}");
    assert!((b.width - 16.0 / 160.0).abs() < 0.02, "{b:?}");
    assert!((b.area - std::f32::consts::PI * 64.0 / (160.0 * 90.0)).abs() < 0.002, "{b:?}");
    assert!((b.color[0] - RED[0]).abs() < 1e-4, "{b:?}");
}

/// Two discs crossing paths, a frame at a time: each keeps its id.
#[test]
fn ids_follow_moving_blobs() {
    let mut t = Tracker::new();
    let s = settings();
    t.update(&room(), 0.016, &s);
    let mut ids_at_start = Vec::new();
    for step in 0..40 {
        let f = step as f32;
        let mut img = room();
        disc(&mut img, 30.0 + 2.0 * f, 30.0, 6.0, RED);
        disc(&mut img, 130.0 - 2.0 * f, 60.0, 6.0, [0.1, 0.8, 0.1]);
        let blobs = t.update(&img, 0.016, &s);
        assert_eq!(blobs.len(), 2, "step {step}");
        let red = blobs.iter().find(|b| b.color[0] > 0.5).map(|b| b.id);
        let green = blobs.iter().find(|b| b.color[1] > 0.5).map(|b| b.id);
        if step == 0 {
            ids_at_start = vec![red, green];
        }
        assert_eq!(vec![red, green], ids_at_start, "step {step}");
    }
}

#[test]
fn speed_is_measured_and_predicts_fast_motion() {
    let mut t = Tracker::new();
    let s = Settings { reach: 0.05, ..settings() };
    t.update(&room(), 0.016, &s);
    let mut last: Vec<Blob> = Vec::new();
    // 6 px a frame is 0.0375 picture widths; within reach only with the speed's help after a
    // few frames, so start slower.
    for step in 0..30 {
        let x = 20.0 + (step as f32 * 0.4).min(6.0) * step as f32 * 0.5;
        let mut img = room();
        disc(&mut img, x.min(150.0), 45.0, 5.0, RED);
        last = t.update(&img, 1.0 / 60.0, &s);
        assert_eq!(last.len(), 1);
        assert_eq!(last[0].id, 1, "step {step}");
    }
    assert!(last[0].vx > 0.5, "{:?}", last[0]);
    assert!(last[0].age > 0.4, "{:?}", last[0]);
}

#[test]
fn a_short_gap_keeps_the_id_and_a_long_one_does_not() {
    let mut t = Tracker::new();
    let s = Settings { hold: 0.1, ..settings() };
    t.update(&room(), 0.016, &s);
    let mut img = room();
    disc(&mut img, 80.0, 45.0, 6.0, RED);
    assert_eq!(t.update(&img, 0.02, &s)[0].id, 1);
    for _ in 0..3 {
        assert!(t.update(&room(), 0.02, &s).is_empty());
    }
    assert_eq!(t.update(&img, 0.02, &s)[0].id, 1, "back after 60 ms");
    for _ in 0..10 {
        t.update(&room(), 0.02, &s);
    }
    assert_eq!(t.update(&img, 0.02, &s)[0].id, 2, "back after 200 ms: a new blob");
}

#[test]
fn size_limits_and_the_count_limit() {
    let mut t = Tracker::new();
    let s = Settings { open: 0, close: 0, min_area: 0.002, max_area: 0.05, max_blobs: 2, ..settings() };
    t.update(&room(), 0.016, &s);
    let mut img = room();
    disc(&mut img, 20.0, 20.0, 1.0, RED); // 4 px: too small
    disc(&mut img, 60.0, 45.0, 40.0, RED); // most of the picture: too large
    let blobs = t.update(&img, 0.016, &s);
    assert!(blobs.is_empty(), "{blobs:?}");
    let mut img = room();
    for i in 0..4 {
        disc(&mut img, 20.0 + 35.0 * i as f32, 45.0, 3.0 + i as f32, RED);
    }
    let blobs = t.update(&img, 0.016, &s);
    assert_eq!(blobs.len(), 2);
    assert!(blobs.iter().all(|b| b.x > 0.5), "the two largest: {blobs:?}");
}

#[test]
fn specks_of_noise_are_opened_away() {
    let mut t = Tracker::new();
    let s = Settings { min_area: 0.0, ..settings() };
    t.update(&room(), 0.016, &s);
    let mut img = room();
    for i in 0..30 {
        img.set((i * 37) % 160, (i * 53) % 90, [0.9; 3]);
    }
    assert!(t.update(&img, 0.016, &s).is_empty());
}

#[test]
fn the_background_adapts_to_a_lasting_change() {
    let mut t = Tracker::new();
    let s = Settings { adapt: 0.5, max_area: 1.0, ..Settings::default() };
    t.update(&room(), 0.016, &s);
    let lit = Image::filled(160, 90, [0.35; 3]);
    let first = t.update(&lit, 0.016, &s);
    assert_eq!(first.len(), 1, "the whole room changed: {first:?}");
    let mut blobs = first;
    // While it is a blob it adapts ten times slower (5 s), until the difference falls below
    // the threshold (after about 2 s), then at the full rate.
    for _ in 0..240 {
        blobs = t.update(&lit, 1.0 / 60.0, &s);
    }
    assert!(blobs.is_empty(), "after four seconds the light is background");
    let bg = t.background().map(|b| b.data[0][0]).unwrap_or(0.0);
    assert!((bg - 0.35).abs() < 0.02, "{bg}");
}

#[test]
fn learning_takes_the_next_frame() {
    let mut t = Tracker::new();
    let s = settings();
    t.update(&room(), 0.016, &s);
    let mut img = room();
    disc(&mut img, 80.0, 45.0, 6.0, RED);
    assert_eq!(t.update(&img, 0.016, &s).len(), 1);
    t.learn();
    assert!(t.update(&img, 0.016, &s).is_empty());
    assert_eq!(t.update(&room(), 0.016, &s).len(), 1, "now the empty room differs");
}

#[test]
fn bright_mode_needs_no_background() {
    let mut t = Tracker::new();
    let s = Settings { detect: Detect::Bright, threshold: 0.5, ..settings() };
    let mut img = room();
    disc(&mut img, 100.0, 20.0, 4.0, [1.0, 1.0, 1.0]);
    let blobs = t.update(&img, 0.016, &s);
    assert_eq!(blobs.len(), 1);
    assert!(t.background().is_none());
}

#[test]
fn hostile_frames_do_not_panic() {
    let mut t = Tracker::new();
    let s = settings();
    t.update(&Image { width: 10, height: 10, data: vec![[0.0; 3]; 3] }, 0.016, &s);
    t.update(&Image { width: 0, height: 0, data: Vec::new() }, f32::NAN, &s);
    let nan = Image::filled(8, 8, [f32::NAN, f32::INFINITY, -1.0]);
    t.update(&nan, 0.016, &s);
    t.update(&room(), 0.016, &Settings { threshold: f32::NAN, reach: f32::INFINITY, ..s.clone() });
    assert!(Image::new(3, 3, vec![[0.0; 3]; 8]).is_none());
}
