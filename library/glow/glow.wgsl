// An object glowing from inside, as if a candle burned in it: no light on the room, only the
// object's own skin, brightest round the flame low in the middle and darker towards its
// silhouette, where the skin turns away; the furrows (dark lines in the camera's photo, see the
// furrows effect) glow hotter and more yellow, as light leaking through thin skin. All of it
// flickers as one flame, swaying a little, and it lights up with a few sputters when the effect
// starts. The colour runs from deep orange (dim) to yellow (bright), as a flame's does.
// in0: a labels picture (green the object), in1: the distance from the object's silhouette
// (scan_labels' `sdf`), in2: the photo on the object lightly softened, in3: the same blurred
// wide (photo_detail's `fine` and `wide`).
// @param clock float = -1 -1..100000 -- seconds since the effect started (bind a chan.clock); below 0: the show time
// @param box vec4 = [0, 0, 1920, 1080] -- the object's bounding box in the camera picture: left, top, right, bottom
// @param flame vec2 = [0.5, 0.62] -- where the flame sits in the box, as shares across and down
// @param spread float = 1 0.1..2 -- how far the flame's light reaches, in half the box's size
// @param skin float = 0.8 0..2 -- brightness of the skin at the flame
// @param furrow float = 2.2 0..3 -- brightness of the furrows
// @param threshold float = 0.05 0..0.5 -- how much darker than around a furrow must be in the photo to glow
// @param full float = 0.15 0..1 -- darker by this much: the furrow glows in full
// @param edge float = 45 0..200 -- the skin dims this close to the silhouette, pixels
// @param flicker float = 0.35 0..1 -- how deep the flame's flicker goes
// @param sway float = 0.05 0..0.3 -- how far the flame sways, share of the box's width
// @param ignite float = 2 0..10 -- seconds the candle takes to light up when the effect starts
// @param dim color = [1, 0.28, 0.02, 1] -- the glow where it is faint
// @param hot color = [1, 0.78, 0.3, 1] -- the glow where it is bright

fn grey(c: vec4f) -> f32 {
    return dot(c.rgb, vec3f(0.2126, 0.7152, 0.0722)) / max(c.a, 1e-4);
}

// The flame's brightness over time, about 0.6 to 1.1 (shared with glow_rim.wgsl: same noises).
fn flame(t: f32, depth: f32) -> f32 {
    let n = 0.55 * pn_noise(vec3f(t * 3.1, 0.0, 0.0), 51u) + 0.3 * pn_noise(vec3f(t * 9.0, 1.0, 0.0), 52u)
        + 0.15 * pn_noise(vec3f(t * 21.0, 2.0, 0.0), 53u);
    // Now and then the flame gutters for a moment.
    let gutter = smoothstep(0.45, 0.75, pn_noise(vec3f(t * 0.45, 3.0, 0.0), 54u));
    return 1.0 + depth * (0.6 * n - 0.45 * gutter);
}

// Lighting up: a few sputters, then the full flame.
fn lit(t: f32, secs: f32) -> f32 {
    if (secs <= 0.0) {
        return 1.0;
    }
    let s = clamp(t / secs, 0.0, 1.0);
    let sputter = 0.5 + 0.5 * pn_noise(vec3f(t * 14.0, 4.0, 0.0), 55u);
    return mix(s * s * sputter, 1.0, s * s);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = vec2i(v.pos.xy);
    if (i32(round(textureLoad(in0, p, 0).g * 255.0)) == 0) {
        return vec4f(0.0);
    }
    let t = select(pn_time.seconds, params.clock, params.clock >= 0.0);
    let lo = params.box.xy;
    let size = max(params.box.zw - params.box.xy, vec2f(1.0));
    let sway = params.sway * size.x * pn_noise(vec3f(t * 1.1, 6.0, 0.0), 56u);
    let at = lo + params.flame * size + vec2f(sway, 0.0);
    let r = length((v.pos.xy - at) / (0.5 * size * params.spread));
    let body = exp(-r * r);
    let inside = max(-textureSampleLevel(in1, pn_clamp, v.uv, 0.0).r * f32(textureDimensions(in1).x), 0.0);
    let turn = smoothstep(0.0, max(params.edge, 1.0), inside);
    // The furrows, away from the silhouette (the rim is darker than around it too).
    var leak = 0.0;
    let fine = textureLoad(in2, p, 0);
    let wide = textureLoad(in3, p, 0);
    if (fine.a > 0.5 && wide.a > 0.5) {
        let dark = (grey(wide) - grey(fine)) / max(grey(wide), 1e-3);
        leak = smoothstep(params.threshold, params.threshold + params.full, dark) * smoothstep(15.0, 35.0, inside);
    }
    let f = flame(t, params.flicker) * lit(t, params.ignite);
    let i = f * (params.skin * (0.08 + body) * turn + params.furrow * leak * (0.35 + 0.65 * body));
    let c = mix(params.dim.rgb, params.hot.rgb, smoothstep(0.25, 1.1, i)) * i;
    return vec4f(c, clamp(i, 0.0, 1.0));
}
