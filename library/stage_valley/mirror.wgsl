// The lake's mirror of what stands on the water, for lake.wgsl's in3. in0 is a render by a camera
// mirrored at the water (as far below it as the stage's camera is above, tilted the other way, the
// roll turned round). This pass turns it upside down, so each pixel shows what the water at the
// same pixel of the stage's picture mirrors. It cuts away what lies below the water as that camera
// sees it, fades the mirror with distance, and breaks it up with the lake's ripples.
// The ripples shift the mirror as on real water: by nothing where a thing meets the water, more
// the further the mirrored point lies from the water it is seen in. So the mirror of a reed or a
// stone starts right at its foot, and only its far parts sway.
// in0: the mirrored render's picture, in1: its data output (the distance from the camera in alpha,
// 0 where nothing was drawn), in2: the stage's lane picture (lane.wgsl), optional: alpha is the
// shore, where the ripples are rougher, as in lake.wgsl.
// Render it the size of the stage's picture; the ripples here must match lake.wgsl's.
// @param start float = 12 0..50 -- metres where the fade begins
// @param end float = 40 0..100 -- metres where nothing is left
// @param eye float = 1.2 0..10 -- the stage camera's height over the water, metres
// @param pitch float = -1 -60..60 -- the stage camera's tilt up, degrees
// @param fov float = 58 10..120 -- the stage camera's vertical field of view, degrees
// @param roll float = 0 -45..45 -- the stage camera's roll, degrees
// @param side float = 0 -45..45 -- the stage camera's turn to the right, degrees
// @param reach float = 40 1..200 -- metres ahead where the floor fades out: the horizon lies there
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param step float = 0.5 0.1..2 -- metres the floor moves per beat
// @param ripple float = 1 0..3 -- how much the ripples break up the mirror (the lake's `ripple`)

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;
const TAPS: i32 = 6;
// What stands on the water mirrors with less of the vertical blur than the sky, so thin rims stay
// lines; a small sideways wobble breaks them into ripples instead.
const THING_SPREAD: f32 = 0.5;
const THING_WOBBLE: f32 = 0.04;

fn tan_down() -> f32 {
    return tan(atan(max(params.eye, 0.0) / max(params.reach, 0.1)) + radians(params.pitch));
}

// As lake.wgsl: where a pixel lies in the sky, in picture heights of a 60 degree view, and back.
fn sky_at(px: vec2f) -> vec3f {
    let res = pn.resolution;
    let t = tan(radians(0.5 * clamp(params.fov, 1.0, 170.0)));
    let q = vec2f((px.x - 0.5 * res.x) / res.y, 0.5 - px.y / res.y) * 2.0 * t;
    let r = radians(-params.roll);
    let p = vec2f(cos(r) * q.x - sin(r) * q.y, sin(r) * q.x + cos(r) * q.y);
    let unit = 2.0 * tan(radians(30.0));
    return vec3f(vec2f(p.x + tan(radians(params.side)), p.y + tan_down()) / unit, 2.0 * t / (res.y * unit));
}

fn sky_uv(s: vec2f) -> vec2f {
    let res = pn.resolution;
    let t = tan(radians(0.5 * clamp(params.fov, 1.0, 170.0)));
    let unit = 2.0 * tan(radians(30.0));
    let p = vec2f(s.x * unit - tan(radians(params.side)), s.y * unit - tan_down());
    let r = radians(-params.roll);
    let q = vec2f(cos(r) * p.x + sin(r) * p.y, -sin(r) * p.x + cos(r) * p.y);
    let px = vec2f(q.x / (2.0 * t) * res.y + 0.5 * res.x, (0.5 - q.y / (2.0 * t)) * res.y);
    return px / res;
}

// The ray of the stage's camera through a point of its picture (uv), its forward part 1, so a
// point at view distance d lies d along it.
fn ray_at(uv: vec2f) -> vec3f {
    let t = tan(radians(0.5 * clamp(params.fov, 1.0, 170.0)));
    let aspect = pn.resolution.x / max(pn.resolution.y, 1.0);
    let q = vec2f((uv.x - 0.5) * aspect, 0.5 - uv.y) * 2.0 * t;
    let r = radians(-params.roll);
    let p = vec2f(cos(r) * q.x - sin(r) * q.y, sin(r) * q.x + cos(r) * q.y);
    let ahead = normalize(vec3f(tan(radians(params.side)), 1.0, tan(radians(params.pitch))));
    let right = normalize(vec3f(ahead.y, -ahead.x, 0.0));
    let up = cross(right, ahead);
    return ahead + p.x * right + p.y * up;
}

// The mirrored render's distance at a point of the stage's picture, 0 where nothing was drawn. The
// data pass has no antialiasing, so the soft edge of a line can fall on a texel it left empty:
// there the nearest distance around stands in.
fn distance_at(uv: vec2f) -> f32 {
    let last = vec2i(textureDimensions(in1)) - 1;
    let at = clamp(vec2i(vec2f(uv.x, 1.0 - uv.y) * vec2f(textureDimensions(in1))), vec2i(0), last);
    var d = textureLoad(in1, at, 0).a;
    if d <= 0.0 {
        d = 1e9;
        for (var dy = -1; dy <= 1; dy++) {
            for (var dx = -1; dx <= 1; dx++) {
                let n = textureLoad(in1, clamp(at + vec2i(dx, dy), vec2i(0), last), 0).a;
                if n > 0.0 {
                    d = min(d, n);
                }
            }
        }
        if d > 1e8 {
            d = 0.0;
        }
    }
    return d;
}

// The mirror at a point of the stage's picture: the mirrored render turned upside down, cut where
// its point lies below the water, faded with distance.
fn mirror_at(uv: vec2f) -> vec4f {
    let c = textureSampleLevel(in0, pn_clamp, vec2f(uv.x, 1.0 - uv.y), 0.0);
    let d = distance_at(uv);
    if d <= 0.0 {
        // A faint haze the data pass leaves out keeps its colour.
        return select(vec4f(0.0), c, c.a < 0.05);
    }
    // The point lies d along the stage camera's ray as the mirror image: as far below the water as
    // the real one stands above it.
    let height = -(max(params.eye, 0.0) + min(d, 1e4) * ray_at(uv).z);
    let above = smoothstep(-0.02, 0.02, height);
    return c * above * (1.0 - smoothstep(params.start, params.end, d));
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let uv = v.pos.xy / pn.resolution;
    let at = sky_at(v.pos.xy);
    let px = max(at.z, 1e-5);
    let ray = ray_at(uv);
    if at.y > px || ray.z >= 0.0 {
        return mirror_at(uv);
    }
    let unit = 2.0 * tan(radians(30.0));
    let b = max(-at.y, 0.0);

    // The water this pixel sees, as lake.wgsl finds it.
    let angle = -radians(params.pitch) - atan(at.y * unit - tan_down());
    let eye = max(params.eye, 0.05);
    let dist = clamp(eye / tan(max(angle, 1e-4)), 0.0, 1000.0);
    let across = at.x * unit * dist;
    let along = dist + params.beats * params.step;
    var shore_here = 1.0;
    if textureLoad(in2, vec2i(0, 0), 0).b > 0.0 {
        var sum = 0.0;
        for (var k = -2; k <= 2; k++) {
            let u = clamp((dist + 0.75 * f32(k) - LANE_NEAR) / (LANE_FAR - LANE_NEAR), 0.0, 1.0);
            sum += textureSampleLevel(in2, pn_clamp, vec2f(u, 0.5), 0.0).a;
        }
        shore_here = clamp(sum / 5.0, 0.0, 1.0);
    }

    // How far the ripples may move the mirror here: 0 where the mirrored point lies at the water
    // itself (a thing's foot), towards 1 for a point far beyond the water it is seen in. The water
    // lies `water` along this pixel's ray, the mirrored point `d`.
    let water = max(params.eye, 0.0) / max(-ray.z, 1e-4);
    let d = distance_at(uv);
    let reach = select(1.0, clamp(1.0 - water / max(d, 1e-4), 0.0, 1.0), d > 0.0);

    // The lake's ripples, the same noise at the same water.
    let rough = params.ripple * mix(0.5, 1.0, shore_here);
    let w1 = pn_noise(vec3f(across * 0.35, along * 1.3, params.beats * 0.05), 3u);
    let w2 = pn_noise(vec3f(across * 0.9, along * 3.1, params.beats * 0.08), 5u);
    let w3 = pn_noise(vec3f(across * 2.2, along * 7.0, params.beats * 0.2), 11u);
    let shift = reach * rough * (0.30 * w1 + 0.15 * w2) * (b + 2.0 * px);
    let sway = reach * rough * (0.015 * w2 + THING_WOBBLE * w3) * (b + 2.0 * px);
    let spread = reach * (0.05 * b + 3.0 * px) * (0.5 + 0.5 * rough) * THING_SPREAD;
    var sum = vec4f(0.0);
    for (var k = 0; k < TAPS; k++) {
        let f = (f32(k) + 0.5) / f32(TAPS) - 0.5;
        let s = vec2f(at.x + sway, max(b + shift + f * spread, 0.0));
        sum += mirror_at(sky_uv(vec2f(s.x, -s.y)));
    }
    return sum / f32(TAPS);
}
