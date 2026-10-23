// Glow from the furrows, as if a candle burned inside the object: where a picture lies below its
// own blurred neighbourhood (a furrow; a crest lies above), it glows, flickering.
// in0: the picture lightly softened, in1: the same blurred wide (photo_detail's `fine` and
// `wide`, transparent off the object), in2: a labels picture (green the object), in3: the
// distance from the silhouette (scan_labels' `sdf`), so the rim does not count as a furrow. The
// picture is either the height (height_camera.exr) or, with `relative` on, the camera's photo of
// the object: a pumpkin's furrows are a few millimetres deep, below a scan's noise, but each one
// is a dark line in the photo.
// @param gain float = 120 0..400 -- how much a furrow's depth counts (height: parallax pixels over the range; photo: the share it is darker than around it)
// @param relative float = 0 0..1 -- 1: depth relative to the brightness around (for a photo), 0: plain difference (for the height)
// @param threshold float = 0.1 0..1 -- depth below which nothing glows
// @param flicker float = 0.5 0..1
// @param color color = [1, 0.45, 0.05, 1]
// @param core color = [1, 0.9, 0.5, 1]
// @param crest float = 0 0..1 -- how dark the crests get
// @param rim float = 30 0..100 -- no glow this close to the silhouette, pixels

// A label of /clean: its red is the face, its green the object, k / 255.
fn label(t: texture_2d<f32>, p: vec2i, channel: i32) -> i32 {
    let size = vec2i(textureDimensions(t));
    let c = textureLoad(t, clamp(p, vec2i(0), size - 1), 0);
    return i32(round(select(c.r, c.g, channel == 1) * 255.0));
}

fn straight(c: vec4f) -> f32 {
    return dot(c.rgb, vec3f(0.2126, 0.7152, 0.0722)) / max(c.a, 1e-4);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = vec2i(v.pos.xy);
    let fine = textureLoad(in0, p, 0);
    let wide = textureLoad(in1, p, 0);
    if (label(in2, p, 1) == 0 || fine.a < 0.5 || wide.a < 0.5) {
        return vec4f(0.0);
    }
    let inside = -textureSampleLevel(in3, pn_clamp, v.uv, 0.0).r * f32(textureDimensions(in3).x);
    let away = smoothstep(params.rim * 0.5, params.rim, inside);
    let around = mix(1.0, max(straight(wide), 1e-3), params.relative);
    let depth = (straight(wide) - straight(fine)) / around * params.gain * away;
    let t = pn_time.seconds;
    let f = 1.0 - params.flicker * (0.5 + 0.5 * (0.6 * pn_noise(vec3f(t * 3.0, 0.0, 0.0), 21u)
        + 0.4 * pn_noise(vec3f(t * 11.0, 5.0, 0.0), 22u)));
    let g = smoothstep(params.threshold, params.threshold + 0.6, depth) * f;
    let rgb = mix(params.color.rgb, params.core.rgb, g * g * 0.6) * g * 1.2;
    let dark = smoothstep(0.0, 0.5, -depth) * params.crest;
    let a = clamp(g + dark, 0.0, 1.0);
    return vec4f(rgb, a);
}
