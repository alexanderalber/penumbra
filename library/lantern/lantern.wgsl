// A carved jack-o'-lantern face on a round object: eyes, nose and a jagged mouth glowing warm
// from within, flickering like a candle, on a dim skin. The face lives on a sphere fitted to the
// object (`center`, `size`, `stem` in camera pixels; the pole at the stem), drawn in its
// longitude and latitude, so it curves with the surface.
// Over one loop (`length`, 28 s): dark, the candle catches and sputters up, the eyes open; calm
// (a soft grin, looks left and right, blinks); sly (narrowed eyes, side glance, a crooked grin);
// scary (slanted angry eyes, slit pupils, a wide jagged mouth that bursts into a laugh); a last
// flare and the candle blows out, dark until the loop starts over.
// `part` picks the layer: 0 the skin and the warm spill on the face the object stands on (meant
// for every projector, blended), 1 the face alone (meant for one projector, unweighted, so a
// blend seam does not cut through it), 2 both (the camera picture).
// in0: a labels picture (scan_labels' `labels`: red the face, green the object, k / 255), in1:
// the regions (8-bit labels through tex.image_file), in2, optional: the camera's still of the scene
// lit white by the projector the face comes from, to even out its light across the face (`even`).
// @param clock float = -1 -1..600 -- seconds since the effect started (bind a chan.clock); below 0: the show time
// @param seek float = 0 0..60 -- seconds added to the clock, to jump into the loop
// @param length float = 28 10..60 -- seconds of one loop
// @param part int = 2 0..2 -- 0 skin and spill, 1 the face, 2 both
// @param mouth float = -1 -1..1 -- how far the mouth opens, 0 to 1 (bind it to an audio level); below 0: the loop's own
// @param object int = 1 0..255 -- the object's label in the labels picture; 0: any object
// @param lid int = 0 0..255 -- the region the object stands on, for the spill; 0: no spill
// @group light
// @param bright float = 1.4 0..3 -- the holes' light
// @param core color = [1, 0.82, 0.32, 1] -- deep inside a hole
// @param edge color = [1, 0.38, 0.03, 1] -- at a hole's cut edge
// @param halo float = 0.35 0..2 -- the glow on the skin around the holes
// @param halo_width float = 0.05 0.005..0.3 -- radians on the sphere
// @param skin float = 0.10 0..1 -- the skin's dim glow
// @param skin_color color = [1, 0.3, 0.04, 1]
// @param spill float = 0.35 0..2 -- the warm light on that region around the object's foot
// @param spill_radius float = 70 5..400 -- pixels from the fitted sphere's outline at which the spill has fallen to a third
// @param flicker float = 0.35 0..1
// @param even float = 0.7 0..1 -- how much the face is brightened where its projector lights the object less than at the face's middle
// @param even_max float = 3 1..6 -- at most this much
// @group face
// @param yaw float = 0.12 -0.6..0.6 -- turns the face round the object, radians (positive: to the camera's right)
// @param drop float = 0.12 -0.6..0.8 -- moves the face down from the point facing the camera, radians on the sphere
// @param scale float = 1 0.5..1.6 -- the face's size
// @param eye_size float = 0.21 0.05..0.5 -- radians
// @param eye_apart float = 0.36 0.1..0.8 -- each eye's distance from the middle, radians
// @param eye_height float = 0.22 -0.3..0.6 -- radians above the face's middle
// @param look_range float = 0.4 0..1 -- how far the pupils move, in eye sizes
// @group shape
// @param center vec2 = [910, 466] -- the object's middle in the camera picture, pixels
// @param size float = 149 50..400 -- its radius, pixels
// @param stem vec2 = [922, 367] -- the stem's base, pixels

const PI: f32 = 3.14159265;

fn label(t: texture_2d<f32>, p: vec2i, channel: i32) -> i32 {
    let size = vec2i(textureDimensions(t));
    let c = textureLoad(t, clamp(p, vec2i(0), size - 1), 0);
    return i32(round(select(c.r, c.g, channel == 1) * 255.0));
}

// An 8-bit label from a tex.image_file (the image is read as sRGB, so undo that first).
fn image_label(t: texture_2d<f32>, p: vec2i) -> i32 {
    let size = vec2i(textureDimensions(t));
    let c = textureLoad(t, clamp(p, vec2i(0), size - 1), 0).r;
    let s = select(1.055 * pow(max(c, 0.0), 1.0 / 2.4) - 0.055, c * 12.92, c <= 0.0031308);
    return i32(round(s * 255.0));
}

// How bright the white-lit still is round x (pixels), averaged over a few taps.
fn lit(x: vec2f) -> f32 {
    let size = vec2i(textureDimensions(in2));
    var sum = 0.0;
    for (var i = -2; i <= 2; i++) {
        for (var j = -2; j <= 2; j++) {
            let p = clamp(vec2i(round(x)) + vec2i(i, j) * 6, vec2i(0), size - 1);
            sum += pn_luma(textureLoad(in2, p, 0).rgb);
        }
    }
    return sum / 25.0;
}

// Signed distance to a triangle, negative inside.
fn sd_tri(p: vec2f, p0: vec2f, p1: vec2f, p2: vec2f) -> f32 {
    let e0 = p1 - p0;
    let e1 = p2 - p1;
    let e2 = p0 - p2;
    let v0 = p - p0;
    let v1 = p - p1;
    let v2 = p - p2;
    let q0 = v0 - e0 * clamp(dot(v0, e0) / dot(e0, e0), 0.0, 1.0);
    let q1 = v1 - e1 * clamp(dot(v1, e1) / dot(e1, e1), 0.0, 1.0);
    let q2 = v2 - e2 * clamp(dot(v2, e2) / dot(e2, e2), 0.0, 1.0);
    let s = sign(e0.x * e2.y - e0.y * e2.x);
    let d = min(min(vec2f(dot(q0, q0), s * (v0.x * e0.y - v0.y * e0.x)),
                    vec2f(dot(q1, q1), s * (v1.x * e1.y - v1.y * e1.x))),
                vec2f(dot(q2, q2), s * (v2.x * e2.y - v2.y * e2.x)));
    return -sqrt(d.x) * sign(d.y);
}

// 0 before a, 1 after b, smooth between.
fn ramp(t: f32, a: f32, b: f32) -> f32 {
    return smoothstep(a, b, t);
}

// 1 between a and b, with smooth edges `e` seconds long.
fn window(t: f32, a: f32, b: f32, e: f32) -> f32 {
    return smoothstep(a - e, a, t) * (1.0 - smoothstep(b, b + e, t));
}

// The loop's state at t seconds: what the face does.
struct Face {
    level: f32,   // the candle, 0 dark to 1
    expr: f32,    // 0 calm, 1 sly, 2 scary
    lid: f32,     // 0 eyes open, 1 shut
    look: f32,    // the pupils, -1 left to 1 right
    open: f32,    // the mouth's opening, radians
    laugh: f32,   // 0 to 1 while laughing
}

fn timeline(t: f32) -> Face {
    var f: Face;
    let tt = pn_time.seconds;
    // The candle catches between 1 and 4 s: sputters, dropouts that get rarer, then steady. It
    // flares at 24 s and blows out at about 25 s, with a few last sputters.
    let catching = ramp(t, 1.0, 4.2);
    let drop = smoothstep(0.1, 0.5, pn_noise(vec3f(tt * 9.0, 4.0, 0.0), 41u) + 0.9 * catching - 0.2);
    var level = select(0.0, catching * mix(drop, 1.0, catching * catching), t > 1.0);
    let dying = ramp(t, 24.9, 26.2);
    let gasp = smoothstep(0.0, 0.4, pn_noise(vec3f(tt * 11.0, 5.0, 0.0), 42u)) * (1.0 - dying);
    level *= mix(1.0, gasp * 0.6, ramp(t, 24.9, 25.1));
    level *= 1.0 - dying;
    level *= 1.0 + 0.5 * window(t, 24.0, 24.6, 0.25);
    f.level = level;

    f.expr = ramp(t, 10.0, 11.5) + ramp(t, 15.5, 17.0);

    // Eyes open as the candle settles; blinks; shut as it dies.
    var lid = 1.0 - ramp(t, 3.4, 4.6);
    let blinks = array<f32, 6>(5.6, 8.0, 9.9, 12.6, 18.0, 22.8);
    for (var i = 0; i < 6; i++) {
        let d = abs(t - blinks[i]);
        // A slow, knowing blink while sly.
        let w = select(0.09, 0.22, i == 3);
        lid = max(lid, 1.0 - smoothstep(w * 0.5, w * 1.6, d));
    }
    f.lid = lid;

    // Calm: a look left, then right. Sly: a side glance that holds, a flick across and back.
    // Scary: a stare, jittering while it laughs.
    var look = -window(t, 6.6, 7.6, 0.25) + window(t, 8.3, 9.3, 0.25);
    look += 0.9 * window(t, 11.0, 13.4, 0.4) - 0.9 * window(t, 13.9, 14.7, 0.2) + 0.6 * window(t, 14.9, 15.4, 0.2);
    let laugh = window(t, 18.6, 23.4, 0.5);
    look += 0.15 * laugh * pn_noise(vec3f(tt * 6.0, 6.0, 0.0), 43u);
    f.look = clamp(look, -1.0, 1.0);

    // The mouth: a closed grin, a sly half grin, a wide snarl, and the laugh, a fast open and
    // shut that starts slow.
    let rate = mix(2.5, 4.5, ramp(t, 18.6, 20.0));
    let ha = 0.5 + 0.5 * sin(2.0 * PI * rate * (t - 18.6));
    var open = mix(0.05, 0.04, ramp(t, 10.0, 11.5)) + 0.07 * ramp(t, 15.5, 17.0);
    open += 0.03 * window(t, 9.0, 9.6, 0.2);
    open += laugh * (0.04 + 0.11 * ha);
    f.laugh = laugh;
    f.open = open;
    return f;
}

// The candle's flicker, about 1 - flicker .. 1.
fn candle(tt: f32) -> f32 {
    let n = 0.55 * pn_noise(vec3f(tt * 4.0, 1.0, 0.0), 32u) + 0.3 * pn_noise(vec3f(tt * 13.0, 2.0, 0.0), 33u)
        + 0.15 * pn_noise(vec3f(tt * 29.0, 3.0, 0.0), 34u);
    return 1.0 - params.flicker * (0.5 + 0.5 * n);
}

// The eye on side `side` (1 right, -1 left): signed distance in radians, negative inside the
// lit part (the hole without its pupil).
fn eye(uv: vec2f, side: f32, f: Face) -> f32 {
    let s = params.eye_size * params.scale;
    let c = vec2f(side * params.eye_apart * params.scale, params.eye_height * params.scale);
    // In eye sizes, x outwards from the nose, y up.
    let p = vec2f((uv.x - c.x) * side, uv.y - c.y) / s;
    // Calm: a triangle, point up. Sly: point down, the lid flat and tilted. Scary: the top edge
    // slanting down to the nose, the eye bigger.
    let e1 = clamp(f.expr, 0.0, 1.0);
    let e2 = clamp(f.expr - 1.0, 0.0, 1.0);
    let a = mix(mix(vec2f(-0.95, -0.55), vec2f(-1.0, 0.1), e1), vec2f(-1.0, -0.15), e2);
    let b = mix(mix(vec2f(0.95, -0.55), vec2f(1.0, 0.38), e1), vec2f(1.1, 0.9), e2);
    let d = mix(mix(vec2f(0.0, 0.85), vec2f(0.15, -0.5), e1), vec2f(0.15, -0.75), e2);
    var dist = sd_tri(p, a, b, d) - 0.06;
    // The lid comes down from above the eye's top.
    let top = max(max(a.y, b.y), d.y) + 0.1;
    let bottom = min(min(a.y, b.y), d.y) - 0.1;
    let line = mix(top, bottom, clamp(f.lid, 0.0, 1.0));
    dist = max(dist, p.y - line);
    // The pupil: round, then a slit as it turns scary; it follows `look` (in camera x, so the
    // left eye's local x is flipped).
    let centre = (a + b + d) / 3.0;
    let pc = centre + vec2f(f.look * side * params.look_range, -0.05);
    let r = vec2f(mix(0.24, 0.08, e2), mix(0.24, 0.36, e2));
    let q = (p - pc) / r;
    let pupil = (length(q) - 1.0) * min(r.x, r.y);
    dist = max(dist, -pupil);
    return dist * s;
}

fn nose(uv: vec2f, f: Face) -> f32 {
    let s = params.scale;
    let flare = 1.0 + 0.25 * f.laugh;
    let w = 0.075 * s * flare;
    return sd_tri(uv - vec2f(0.0, -0.02 * s), vec2f(-w, -0.07 * s), vec2f(w, -0.07 * s), vec2f(0.0, 0.07 * s)) - 0.008;
}

// A zigzag or blocky row of teeth along x (-1..1), 0 to 1.
fn teeth(x: f32, count: f32, sharp: f32, shift: f32) -> f32 {
    let g = fract((x + 1.0) * count * 0.5 + shift);
    let block = smoothstep(0.22, 0.28, g) * (1.0 - smoothstep(0.52, 0.58, g));
    let zig = 1.0 - abs(2.0 * g - 1.0);
    return mix(block, zig, sharp);
}

// The mouth: a crescent between an upper and a lower curve, with teeth from both. Negative
// inside, roughly radians.
fn mouth_dist(uv: vec2f, f: Face) -> f32 {
    let s = params.scale;
    let e1 = clamp(f.expr, 0.0, 1.0);
    let e2 = clamp(f.expr - 1.0, 0.0, 1.0);
    let half = mix(mix(0.6, 0.55, e1), 0.7, e2) * s;
    let x = uv.x / half;
    let y = uv.y / s;
    // Corners up for a grin; sly lifts one corner; scary flatter, wider.
    let curve = mix(mix(0.16, 0.1, e1), 0.07, e2) + 0.05 * f.laugh;
    let tilt = 0.1 * e1 * (1.0 - e2);
    let mid = -0.32 - 0.03 * e2;
    let upper = mid + curve * x * x + tilt * x;
    let ax = min(abs(x), 1.0);
    let open = select(f.open, params.mouth * 0.2 + 0.03, params.mouth >= 0.0);
    // The lower lip bulges below: a crescent with pointed corners.
    let lower = upper - (open + 0.09 * mix(1.0, 0.7, e2)) * pow(1.0 - ax * ax, 0.6);
    let sharp = e2;
    let count = mix(4.0, 7.0, e2);
    let tooth = mix(0.035, 0.05, e2);
    // Teeth only away from the corners.
    let inner = smoothstep(0.95, 0.7, ax);
    let up = upper - tooth * teeth(x, count, sharp, 0.0) * inner;
    let lo = lower + tooth * teeth(x, count, sharp, 0.5) * inner * mix(0.0, 1.0, max(e2, 0.4));
    let inside = min(min(up - y, y - lo), (1.0 - abs(x)) * half / s * 0.5);
    // Beyond a corner, the distance to the corner itself (else the halo runs on along the
    // curves).
    if (abs(x) > 1.0) {
        let cx = sign(x);
        let corner = vec2f(cx * half, (mid + curve + tilt * cx) * s);
        return length(uv - corner);
    }
    return -inside * s;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = vec2i(v.pos.xy);
    var t = select(pn_time.seconds, params.clock, params.clock >= 0.0) + params.seek;
    let span = max(params.length, 1.0);
    t = t - floor(t / span) * span;
    // The script's times are for a 28 s loop; another length stretches them.
    t = t * 28.0 / span;
    let f = timeline(t);
    if (f.level <= 0.0) {
        return vec4f(0.0);
    }
    let light = f.level * candle(pn_time.seconds);
    let want_skin = params.part != 1;
    let want_face = params.part != 0;

    let q = (v.pos.xy - params.center) / params.size;
    var rgb = vec3f(0.0);
    let obj = label(in0, p, 1);
    let on_object = select(obj == params.object, obj > 0, params.object == 0);
    if (!on_object) {
        // Off the object: only the spill on the region it stands on, around its foot.
        if (want_skin && params.object != 0 && params.lid > 0 && image_label(in1, p) == params.lid) {
            let d = max(length(q) - 1.0, 0.0) * params.size / params.spill_radius;
            let falloff = pow(1.0 / (1.0 + d * d), 1.5) * smoothstep(-0.2, 0.5, q.y);
            rgb = params.edge.rgb * params.spill * light * falloff;
        }
        return vec4f(rgb, clamp(max(rgb.r, max(rgb.g, rgb.b)), 0.0, 1.0));
    }

    // The point on the sphere under the pixel (z towards the camera). The face is drawn in a
    // frame on the sphere round its middle `mid`: the point facing the camera, moved `drop`
    // radians down its meridian (the pole at the stem) and turned `yaw` round the pole. Its
    // coordinates are the angles from `mid` along the sphere, to the right and upwards (towards
    // the stem), so the face curves with the surface.
    let pt = vec3f(q, sqrt(max(1.0 - dot(q, q), 0.0)));
    let sq = (params.stem - params.center) / params.size;
    let up = normalize(vec3f(sq, sqrt(max(1.0 - dot(sq, sq), 0.02))));
    let toward = normalize(vec3f(0.0, 0.0, 1.0) - up * up.z);
    let side0 = cross(toward, up);
    let fwd = toward * cos(params.yaw) + side0 * sin(params.yaw);
    let right = cross(fwd, up);
    let lat = asin(clamp(up.z, -1.0, 1.0)) - params.drop;
    let mid = fwd * cos(lat) + up * sin(lat);
    let upward = up * cos(lat) - fwd * sin(lat);
    let n = normalize(pt);
    let along = max(dot(n, mid), 1e-3);
    let uv = vec2f(atan2(dot(n, right), along), atan2(dot(n, upward), along));
    // Edges a pixel or so wide: radians per pixel, larger where the surface turns away.
    let aa = 1.2 / (params.size * max(pt.z, 0.25));
    let facing = smoothstep(0.1, 0.35, pt.z);

    if (want_skin) {
        rgb += params.skin_color.rgb * params.skin * light * (0.4 + 0.6 * pt.z);
    }
    if (want_face) {
        let d = min(min(eye(uv, 1.0, f), eye(uv, -1.0, f)), min(nose(uv, f), mouth_dist(uv, f)));
        let hole = 1.0 - smoothstep(-aa, aa, d);
        // The projector's light here against the face's middle, from its white still (a few
        // taps, so the skin's furrows do not show).
        let here = lit(v.pos.xy);
        let there = lit(params.center + (mid.xy) * params.size);
        let boost = pow(clamp(there / max(here, 1e-4), 1.0, params.even_max), params.even);
        // Deeper inside a hole the light is hotter; the laugh pushes it to the edge colour, a
        // little brighter.
        let deep = smoothstep(0.0, 0.06, -d);
        var col = mix(params.edge.rgb, params.core.rgb, deep * (1.0 - 0.3 * f.laugh));
        col *= params.bright * (0.8 + 0.2 * deep) * (1.0 + 0.25 * f.laugh);
        let glow = params.halo * exp(-max(d, 0.0) / params.halo_width) * (1.0 - hole);
        rgb += (col * hole + params.edge.rgb * glow) * light * facing * boost;
    }
    return vec4f(rgb, clamp(max(rgb.r, max(rgb.g, rgb.b)), 0.0, 1.0));
}
