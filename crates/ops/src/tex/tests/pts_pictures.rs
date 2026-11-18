//! Points and pictures on WARP: `pts.from_tex` in each mode and sampling, for pictures of 8
//! bits, half floats and 32-bit floats; `tex.from_pts` and the round trip back; `pts.lookup_tex`
//! with each edge rule; and a picture drawn as points.

use std::path::Path;

use penumbra_core::Value;
use penumbra_gpu::{AdapterChoice, FLOAT_FORMAT, FORMAT, Gpu, Points};
use penumbra_graph::Op;

use super::geo::{render, rig};
use super::pts::{coverage, drawn, points, read};
use super::{Rig, connect, create, gpu, scratch, set};

/// Writes a picture of 32-bit floats, `w` texels to a row.
fn exr(dir: &Path, name: &str, w: u32, texels: &[[f32; 4]]) {
    let h = texels.len() as u32 / w;
    std::fs::write(dir.join(name), penumbra_gpu::image::encode_exr_rgba32f(w, h, texels).unwrap()).unwrap();
}

fn image(path: &str, file: &str) -> Op {
    create(path, "tex.image_file", &[("file", file.into()), ("values", "data".into())])
}

/// The numbers of a one-number attribute.
fn scalars(rig: &Rig, p: &Points, name: &str) -> Vec<f32> {
    let n = p.count().known().unwrap() as usize;
    p.attr(name).unwrap().buffer().read::<f32>(&rig.gpu).unwrap()[..n].to_vec()
}

fn close(a: &[f32], b: &[f32], what: &str) {
    assert!(a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5), "{what}: {a:?} against {b:?}");
}

/// A 5 by 3 picture: red counts the texels, green the rows, blue is a depth, alpha varies.
fn five_by_three() -> Vec<[f32; 4]> {
    (0..15)
        .map(|i| {
            let (x, y) = ((i % 5) as f32, (i / 5) as f32);
            [i as f32 * 0.1, y, if i == 7 { 0.0 } else { 1.0 + x * 0.5 }, 1.0 - i as f32 / 20.0]
        })
        .collect()
}

#[test]
fn a_picture_becomes_points_in_each_mode() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("pts-from-tex");
    let texels = five_by_three();
    exr(&dir, "p.exr", 5, &texels);
    let ops = [
        image("/pic", "p.exr"),
        create(
            "/pts",
            "pts.from_tex",
            &[("sampling", "step".into()), ("step", Value::Int(1)), ("size", Value::Float(5.0))],
        ),
        connect("/pic", "/pts", 0),
    ];
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    assert_eq!(rig.cook_until_new("/pic", 0).format(), FLOAT_FORMAT);

    // Colour: a grid 5 m wide and 3 m high, a point at the centre of each texel, row 0 on top.
    let p = points(&mut rig, "/pts");
    assert_eq!((p.count().known(), p.dims()), (Some(15), Some([3, 5, 1])));
    let pos = read(&rig, &p, "position");
    close(&pos[0][..3], &[-2.0, 0.0, 1.0], "top left");
    close(&pos[14][..3], &[2.0, 0.0, -1.0], "bottom right");
    assert_eq!(read(&rig, &p, "color"), texels);
    assert_eq!(read(&rig, &p, "uv")[6][..2], [0.3, 0.5]);
    assert_eq!(read(&rig, &p, "normal")[3][..3], [0.0, -1.0, 0.0]);
    close(&scalars(&rig, &p, "weight"), &texels.iter().map(|t| t[3]).collect::<Vec<_>>(), "weight is alpha");
    let b = p.bounds().unwrap();
    assert_eq!((b.min, b.max), ([-2.5, 0.0, -1.5], [2.5, 0.0, 1.5]));

    // Height: lifted towards -y by red times `height`.
    rig.edit(&[
        set("/pts.mode", "height".into()),
        set("/pts.channel", "red".into()),
        set("/pts.height", Value::Float(2.0)),
    ]);
    let p = points(&mut rig, "/pts");
    let y: Vec<f32> = read(&rig, &p, "position").iter().map(|q| q[1]).collect();
    close(&y, &texels.iter().map(|t| -2.0 * t[0]).collect::<Vec<_>>(), "height");
    assert!(p.attr("color").is_some() && p.bounds().is_none());

    // Position: rgb is xyz, alpha the weight, no colour.
    rig.edit(&[set("/pts.mode", "position".into())]);
    let p = points(&mut rig, "/pts");
    let pos = read(&rig, &p, "position");
    for (q, t) in pos.iter().zip(&texels) {
        assert_eq!(q[..3], t[..3]);
    }
    assert!(p.attr("color").is_none());
    assert_eq!(scalars(&rig, &p, "weight")[4], texels[4][3]);

    // Depth: blue metres along +y from a camera at the origin; 90 degrees, so the edges of the
    // view are as far out as the depth. No depth, no weight.
    rig.edit(&[
        set("/pts.mode", "depth".into()),
        set("/pts.channel", "blue".into()),
        set("/pts.fov", Value::Float(90.0)),
    ]);
    let p = points(&mut rig, "/pts");
    let pos = read(&rig, &p, "position");
    let uv = read(&rig, &p, "uv");
    for (k, (q, t)) in pos.iter().zip(&texels).enumerate() {
        let d = t[2];
        let want = [(2.0 * uv[k][0] - 1.0) * (5.0 / 3.0) * d, d, (1.0 - 2.0 * uv[k][1]) * d];
        close(&q[..3], &want, "depth");
    }
    let w = scalars(&rig, &p, "weight");
    assert_eq!((w[7], w[8]), (0.0, texels[8][3]));

    // Sampled down: about 4 points and never more (a row of 3); every 2nd texel; the mean of a
    // cell.
    rig.edit(&[
        set("/pts.mode", "color".into()),
        set("/pts.sampling", "count".into()),
        set("/pts.count", Value::Int(4)),
    ]);
    let p = points(&mut rig, "/pts");
    assert_eq!((p.count().known(), p.dims()), (Some(3), Some([1, 3, 1])));
    rig.edit(&[set("/pts.sampling", "step".into()), set("/pts.step", Value::Int(2))]);
    let p = points(&mut rig, "/pts");
    let reds: Vec<f32> = read(&rig, &p, "color").iter().map(|c| c[0]).collect();
    close(&reds, &[0.0, 0.2, 0.4, 1.0, 1.2, 1.4], "every 2nd texel");
    rig.edit(&[set("/pts.filter", "average".into())]);
    let p = points(&mut rig, "/pts");
    let c = read(&rig, &p, "color");
    // The first cell covers texels 0 and 1 of rows 0 and 1; the middle of the top row 1 to 3.
    close(
        &[c[0][0], c[1][0]],
        &[(0.0 + 0.1 + 0.5 + 0.6) / 4.0, (0.1 + 0.2 + 0.3 + 0.6 + 0.7 + 0.8) / 6.0],
        "mean",
    );
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

#[test]
fn every_picture_format_gives_its_values() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("pts-formats");
    // 8 bits per channel, 3 by 2.
    let bytes: Vec<u8> = (0..6u8).flat_map(|i| [i * 40, 255 - i * 40, 7, 255]).collect();
    let img = photocraft_codecs::Image::from_u8(3, 2, photocraft_codecs::ChannelLayout::Rgba, bytes).unwrap();
    let png = photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &Default::default()).unwrap();
    std::fs::write(dir.join("p8.png"), png).unwrap();
    exr(&dir, "p32.exr", 3, &(0..6).map(|i| [i as f32 + 0.123_456_7, -1.5, 1e5, 0.5]).collect::<Vec<_>>());
    let ops = [
        image("/p8", "p8.png"),
        create(
            "/p16",
            "tex.constant",
            &[
                ("width", Value::Int(3)),
                ("height", Value::Int(2)),
                ("color", Value::Vec4([0.3, 2.5, 0.25, 1.0])),
            ],
        ),
        image("/p32", "p32.exr"),
        create("/pts", "pts.from_tex", &[("sampling", "step".into()), ("step", Value::Int(1))]),
        connect("/p8", "/pts", 0),
    ];
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    for (source, format) in [("/p8", FORMAT), ("/p16", FORMAT), ("/p32", FLOAT_FORMAT)] {
        rig.edit(&[connect(source, "/pts", 0)]);
        let tex = rig.cook_until_new(source, 0);
        assert_eq!(tex.format(), format, "{source}");
        let px = rig.gpu.read(&tex).unwrap();
        let p = points(&mut rig, "/pts");
        assert_eq!((p.count().known(), p.dims()), (Some(6), Some([2, 3, 1])), "{source}");
        assert_eq!(read(&rig, &p, "color"), px.data, "{source}: every texel as the picture holds it");
    }
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

#[test]
fn points_become_a_picture_and_come_back() {
    let Some(gpu) = gpu() else { return };
    let ops = [
        create(
            "/grid",
            "pts.generate",
            &[("rows", Value::Int(3)), ("cols", Value::Int(5)), ("size", Value::Vec3([2.0, 1.0, 1.0]))],
        ),
        create("/noise", "pts.noise", &[("flavour", "vector".into()), ("amplitude", Value::Float(0.3))]),
        connect("/grid", "/noise", 0),
        create("/pic", "tex.from_pts", &[]),
        connect("/noise", "/pic", 0),
        create(
            "/back",
            "pts.from_tex",
            &[("mode", "position".into()), ("sampling", "step".into()), ("step", Value::Int(1))],
        ),
        connect("/pic", "/back", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    let moved = points(&mut rig, "/noise");
    let want = read(&rig, &moved, "position");
    // The grid's own layout: 5 wide, 3 high, every texel a point with alpha 1.
    let tex = rig.cook_until_new("/pic", 0);
    assert_eq!((tex.size(), tex.format()), ((5, 3), FLOAT_FORMAT));
    let px = rig.gpu.read(&tex).unwrap();
    for (t, q) in px.data.iter().zip(&want) {
        assert_eq!(*t, [q[0], q[1], q[2], 1.0]);
    }
    let back = points(&mut rig, "/back");
    assert_eq!((back.count().known(), back.dims()), (Some(15), Some([3, 5, 1])));
    assert_eq!(read(&rig, &back, "position"), want, "the round trip gives the points back exactly");

    // Points without a grid fill rows: 37 points 8 to a row leave 3 texels transparent black.
    rig.edit(&[
        set("/grid.shape", "sphere".into()),
        set("/grid.count", Value::Int(37)),
        set("/pic.width", Value::Int(8)),
    ]);
    let tex = rig.cook_until_new("/pic", rig.version("/pic"));
    assert_eq!(tex.size(), (8, 5));
    let px = rig.gpu.read(&tex).unwrap();
    assert!(px.data[..37].iter().all(|t| t[3] == 1.0));
    assert!(px.data[37..].iter().all(|t| *t == [0.0; 4]));
    let back = points(&mut rig, "/back");
    let w = scalars(&rig, &back, "weight");
    assert_eq!((w.len(), &w[35..]), (40, &[1.0, 1.0, 0.0, 0.0, 0.0][..]));
    // Fewer points in a picture of the same size: the texels they left are cleared.
    rig.edit(&[set("/grid.count", Value::Int(33))]);
    let tex = rig.cook_until_new("/pic", rig.version("/pic"));
    let px = rig.gpu.read(&tex).unwrap();
    assert_eq!(tex.size(), (8, 5));
    assert!(px.data[33..].iter().all(|t| *t == [0.0; 4]), "nothing stale past the count");

    // A scalar is grey, a colour keeps its alpha, an attribute penumbra knows but the points lack
    // holds its default, an unknown one is an error.
    rig.edit(&[set("/pic.attribute", "PointScale".into())]);
    let px = rig.pixels_after("/pic");
    assert_eq!((px.data[0], px.data[39]), ([1.0; 4], [0.0; 4]));
    rig.edit(&[set("/pic.attribute", "heat".into())]);
    rig.cook("/pic");
    let s = rig.cooker.status(rig.id("/pic")).unwrap();
    assert!(s.error.as_deref().is_some_and(|e| e.contains("heat")), "{s:?}");
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

#[test]
fn without_float_filtering_the_picture_is_half_floats() {
    let Ok(gpu) = Gpu::headless_without_float_filtering(AdapterChoice::Software) else { return };
    let ops = [
        create("/grid", "pts.generate", &[("rows", Value::Int(2)), ("cols", Value::Int(3))]),
        create("/pic", "tex.from_pts", &[]),
        connect("/grid", "/pic", 0),
        create(
            "/back",
            "pts.from_tex",
            &[("mode", "position".into()), ("sampling", "step".into()), ("step", Value::Int(1))],
        ),
        connect("/pic", "/back", 0),
    ];
    let mut rig = Rig::new(gpu, None, &ops);
    let grid = points(&mut rig, "/grid");
    let want = read(&rig, &grid, "position");
    let tex = rig.cook_until_new("/pic", 0);
    assert_eq!((tex.size(), tex.format()), ((3, 2), FORMAT));
    let back = points(&mut rig, "/back");
    // Half floats hold these positions (halves and whole numbers) exactly.
    assert_eq!(read(&rig, &back, "position"), want);

    // Other numbers round to the nearest half float, as the CPU rounds them: tiny ones to
    // subnormals, too large ones to infinity.
    rig.edit(&[
        create(
            "/odd",
            "pts.math",
            &[
                ("expression", "vec4f(index01 * 1e-5, 70000.0, -0.1, 0.3333 + index01)".into()),
                ("result", "color".into()),
            ],
        ),
        connect("/grid", "/odd", 0),
        connect("/odd", "/pic", 0),
        set("/pic.attribute", "color".into()),
    ]);
    let odd = super::pts_programs::clean(&mut rig, "/odd");
    let colours = read(&rig, &odd, "color");
    let px = rig.pixels_after("/pic");
    for (t, c) in px.data.iter().zip(&colours) {
        let want = c.map(|v| half::f16::from_f32(v).to_f32());
        assert_eq!(t.map(f32::to_bits), want.map(f32::to_bits), "{c:?}");
    }
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

/// Extends the texel index `x` of a row of `n` as `mode` says; `None` for transparent black.
fn extended(x: i32, n: i32, mode: &str) -> Option<i32> {
    match mode {
        "clamp" => Some(x.clamp(0, n - 1)),
        "repeat" => Some(x.rem_euclid(n)),
        "mirror" => {
            let m = x.rem_euclid(2 * n);
            Some(if m >= n { 2 * n - 1 - m } else { m })
        }
        _ => (0..n).contains(&x).then_some(x),
    }
}

#[test]
fn points_look_up_a_picture_at_their_uv_and_position() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("pts-lookup");
    // Four texels in a row, red 1 to 4.
    exr(&dir, "row.exr", 4, &(1..=4).map(|r| [r as f32, 0.0, 0.0, 1.0]).collect::<Vec<_>>());
    let ops = [
        image("/pic", "row.exr"),
        create("/line", "pts.generate", &[("rows", Value::Int(1)), ("cols", Value::Int(5))]),
        // u from -1.1 to 2.1: two points left of the picture, one in it, two right of it.
        create(
            "/spread",
            "pts.math",
            &[("expression", "vec2f(index01 * 3.2 - 1.1, 0.5)".into()), ("result", "uv".into())],
        ),
        connect("/line", "/spread", 0),
        create(
            "/look",
            "pts.lookup_tex",
            &[("filter", "nearest".into()), ("attribute", "heat".into()), ("channels", "red".into())],
        ),
        connect("/spread", "/look", 0),
        connect("/pic", "/look", 1),
    ];
    let mut rig = Rig::new(gpu, Some(&dir), &ops);
    rig.cook_until_new("/pic", 0);
    super::pts_programs::clean(&mut rig, "/spread");
    let spread = points(&mut rig, "/spread");
    let us: Vec<f32> = read(&rig, &spread, "uv").iter().map(|q| q[0]).collect();
    for mode in ["clamp", "repeat", "mirror", "zero"] {
        rig.edit(&[set("/look.extend", mode.into())]);
        let p = points(&mut rig, "/look");
        let got = scalars(&rig, &p, "heat");
        let want: Vec<f32> = us
            .iter()
            .map(|u| extended((u * 4.0).floor() as i32, 4, mode).map_or(0.0, |x| x as f32 + 1.0))
            .collect();
        assert_eq!(got, want, "{mode} at {us:?}");
    }
    // Linear, repeating: halfway between the last texel and the first again.
    rig.edit(&[
        set("/look.extend", "repeat".into()),
        set("/look.filter", "linear".into()),
        set("/spread.expression", "vec2f(1.0, 0.5)".into()),
    ]);
    super::pts_programs::clean(&mut rig, "/spread");
    let p = points(&mut rig, "/look");
    assert_eq!(scalars(&rig, &p, "heat")[0], 2.5);

    // A position projected onto the front plane over the points' box: the left point reads the
    // left texel. Into a colour, `from` and `to` stretch the values; a size takes luminance.
    rig.edit(&[
        set("/look.lookup", "P".into()),
        set("/look.filter", "nearest".into()),
        set("/look.extend", "clamp".into()),
        set("/look.attribute", "Color".into()),
        set("/look.channels", "rgba".into()),
        set("/look.to", Value::Vec2([0.0, 0.5])),
        connect("/line", "/look", 0),
    ]);
    let p = points(&mut rig, "/look");
    let c = read(&rig, &p, "color");
    assert_eq!((c[0][0], c[2][0], c[4][0]), (0.5, 1.5, 2.0));
    assert!(p.bounds().is_some(), "colours leave the box as it was");
    rig.edit(&[set("/look.attribute", "scale".into()), set("/look.to", Value::Vec2([0.0, 1.0]))]);
    let p = points(&mut rig, "/look");
    assert!((scalars(&rig, &p, "scale")[0] - 0.2126).abs() < 1e-5, "luminance of red 1");
    // The box of `center` and `extent` instead: the points all fall in its left quarter.
    rig.edit(&[
        set("/look.fit", "box".into()),
        set("/look.extent", Value::Vec3([8.0, 1.0, 1.0])),
        set("/look.channels", "red".into()),
    ]);
    let p = points(&mut rig, "/look");
    assert!(scalars(&rig, &p, "scale").iter().all(|v| (2.0..=3.0).contains(v)));
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

#[test]
fn a_picture_drawn_as_points_keeps_its_colours() {
    let Some(gpu) = gpu() else { return };
    let dir = scratch("pts-drawn");
    // Left red, right blue.
    exr(&dir, "two.exr", 2, &[[1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]]);
    let ops = vec![
        image("/pic", "two.exr"),
        create(
            "/pts",
            "pts.from_tex",
            &[("sampling", "step".into()), ("step", Value::Int(1)), ("size", Value::Float(2.0))],
        ),
        connect("/pic", "/pts", 0),
        create("/sprites", "geo.from_pts", &[("size", Value::Float(0.6)), ("sprite", "square".into())]),
        connect("/pts", "/sprites", 0),
        create("/white", "mat.constant", &[]),
        create("/lit", "geo.assign_material", &[]),
        connect("/sprites", "/lit", 0),
        connect("/white", "/lit", 1),
        render("/front", &[("position", Value::Vec3([0.0, -8.0, 0.0])), ("antialias", Value::Bool(false))]),
        connect("/lit", "/front", 0),
    ];
    let mut rig = rig(gpu, Some(&dir), &ops);
    let px = drawn(&mut rig, "/front");
    let (n, b) = coverage(&px);
    assert!(n > 40, "{n}");
    let mid = (b[0] + b[2]) / 2;
    let side = |left: bool| {
        let lit: Vec<[f32; 4]> = (0..px.height)
            .flat_map(|y| (0..px.width).map(move |x| (x, y)))
            .filter(|(x, _)| (*x < mid) == left)
            .filter_map(|(x, y)| px.get(x, y))
            .filter(|p| p[3] > 0.5)
            .collect();
        assert!(!lit.is_empty());
        lit.iter()
            .fold([0.0f32; 3], |a, p| [a[0] + p[0], a[1] + p[1], a[2] + p[2]])
            .map(|v| v / lit.len() as f32)
    };
    let (l, r) = (side(true), side(false));
    assert!(l[0] > 0.9 && l[2] < 0.05, "red on the left: {l:?}");
    assert!(r[2] > 0.9 && r[0] < 0.05, "blue on the right: {r:?}");
    assert_eq!(rig.gpu.errors().count, 0, "{:?}", rig.gpu.errors().last);
}

impl Rig {
    /// `path`'s picture, cooked anew after an edit.
    fn pixels_after(&mut self, path: &str) -> penumbra_gpu::Pixels {
        let v = self.version(path);
        let tex = self.cook_until_new(path, v);
        self.gpu.read(&tex).unwrap()
    }
}
