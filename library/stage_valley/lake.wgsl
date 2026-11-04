// The valley's lake: below the horizon a dark water that mirrors the sky picture in in0 (the
// band and the stars) and what stands on the water (in3), blurred and broken up by ripples, faint near the camera and strong towards
// the horizon. Above the horizon in0 passes unchanged. The ripples and a low mist are fixed to
// the water and come towards the camera with the floor, `step` metres per beat of `beats`. Where
// the shore stands (alpha 1 in the lane picture in in1) the reflection is dimmer and rougher; in a
// gap of the shore (alpha 0) the water lies open and mirrors the sky more clearly.
// in0: the sky (the stage's sky with the sky elements screened over it).
// in1: the stage's lane picture (lane.wgsl), optional: alpha is the shore.
// in2: a smaller, blurred copy of in0, optional: the water mirrors that one, so single stars
// melt into a faint glow in the reflection. Without it the water mirrors in0 itself.
// in3: what stands on the water as the water mirrors it (mirror.wgsl), optional: lined up with this
// picture, premultiplied, already broken up by the same ripples. It covers the sky's reflection, so
// a black silhouette stays black in the water and its rims mirror dimmer than they shine.
// @param eye float = 1.2 0..10 -- the camera's height over the water, metres (height of the stage's pose)
// @param pitch float = -1 -60..60 -- the camera's tilt up, degrees
// @param fov float = 58 10..120 -- the camera's vertical field of view, degrees
// @param roll float = 0 -45..45 -- the camera's roll, degrees
// @param side float = 0 -45..45 -- the camera's turn to the right, degrees
// @param reach float = 40 1..200 -- metres ahead where the floor fades out: the horizon lies there
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param step float = 0.5 0.1..2 -- metres the floor moves per beat
// @param energy float = 0 0..1 -- the bass, curved: thickens the mist
// @param warmth float = 0 0..1 -- 0 the ember palette, 1 the jade palette (a greener mist and water)
// @param ground float = 1 0..1 -- how much of the sky picture itself is hidden below the horizon: 1 all of it (only the water shows), 0 none
// @param reflection float = 1 0..2 -- how strongly the water mirrors the sky
// @param ripple float = 1 0..3 -- how much the ripples break up the reflection
// @param mist float = 1 0..3 -- the mist on the water near the camera; 0 none
// @param shore float = 0.5 0..1 -- how much of the reflection shows where the shore stands: 1 as in open water
// @param mirror float = 1 0..1 -- how much the water mirrors what stands on it (in3): 0 only the sky

fn tan_down() -> f32 {
    return tan(atan(max(params.eye, 0.0) / max(params.reach, 0.1)) + radians(params.pitch));
}

// Where this pixel lies in the sky, in picture heights of a 60 degree view: x from straight ahead,
// y up from the horizon. `.z`: one pixel in the same unit. The sky elements' helper.
fn sky_at(px: vec2f) -> vec3f {
    let res = pn.resolution;
    let t = tan(radians(0.5 * clamp(params.fov, 1.0, 170.0)));
    let q = vec2f((px.x - 0.5 * res.x) / res.y, 0.5 - px.y / res.y) * 2.0 * t;
    let r = radians(-params.roll);
    let p = vec2f(cos(r) * q.x - sin(r) * q.y, sin(r) * q.x + cos(r) * q.y);
    let unit = 2.0 * tan(radians(30.0));
    return vec3f(vec2f(p.x + tan(radians(params.side)), p.y + tan_down()) / unit, 2.0 * t / (res.y * unit));
}

// The pixel at a place in the sky, sky_at turned round; returned as uv.
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

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;
const TAPS: i32 = 6;
// How bright the mirrored rims are against the rims themselves, before the fresnel.
const RIM_MIRROR: f32 = 0.6;
// Bright rims mirror relatively dimmer, so a ring of light does not mirror as a glowing disc.
const THING_KNEE: f32 = 2.0;
// The mirror of things fades faster than the sky's as the water is seen further down: a thing
// high in the air mirrors close to the camera, where real water shows little of it.
const THING_FALL: f32 = 4.0;

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let here = textureLoad(in0, vec2i(v.pos.xy), 0);
    let at = sky_at(v.pos.xy);
    let px = max(at.z, 1e-5);
    if at.y > px {
        return here;
    }
    let unit = 2.0 * tan(radians(30.0));
    let jade = clamp(params.warmth, 0.0, 1.0);
    let b = max(-at.y, 0.0);

    // The water this pixel sees: its angle below the level, its distance ahead and to the side.
    let angle = -radians(params.pitch) - atan(at.y * unit - tan_down());
    let eye = max(params.eye, 0.05);
    let dist = clamp(eye / tan(max(angle, 1e-4)), 0.0, 1000.0);
    let across = at.x * unit * dist;
    let along = dist + params.beats * params.step;

    // The shore at that distance: 1 where it stands, 0 in open water.
    var shore_here = 1.0;
    if textureLoad(in1, vec2i(0, 0), 0).b > 0.0 {
        // Over 3 m along the water, so the edge of open water is soft.
        var sum = 0.0;
        for (var k = -2; k <= 2; k++) {
            let u = clamp((dist + 0.75 * f32(k) - LANE_NEAR) / (LANE_FAR - LANE_NEAR), 0.0, 1.0);
            sum += textureSampleLevel(in1, pn_clamp, vec2f(u, 0.5), 0.0).a;
        }
        shore_here = clamp(sum / 5.0, 0.0, 1.0);
    }

    // Ripples, fixed to the water: they shift the mirrored point up and down, more near the camera.
    let rough = params.ripple * mix(0.5, 1.0, shore_here);
    let w1 = pn_noise(vec3f(across * 0.35, along * 1.3, params.beats * 0.05), 3u);
    let w2 = pn_noise(vec3f(across * 0.9, along * 3.1, params.beats * 0.08), 5u);
    let shift = rough * (0.30 * w1 + 0.15 * w2) * (b + 2.0 * px);
    let sway = rough * 0.015 * w2 * (b + 2.0 * px);
    // A blur along the vertical that grows towards the camera, as on real water.
    let spread = (0.05 * b + 3.0 * px) * (0.5 + 0.5 * rough);
    let soft = textureDimensions(in2).x > 1u;
    var mirrored = vec3f(0.0);
    for (var k = 0; k < TAPS; k++) {
        let f = (f32(k) + 0.5) / f32(TAPS) - 0.5;
        let s = vec2f(at.x + sway, max(b + shift + f * spread, 0.0));
        let uv = sky_uv(s);
        var tap = textureSampleLevel(in0, pn_clamp, uv, 0.0).rgb;
        if soft {
            // Close under the horizon the band's thin core still mirrors sharply.
            let blurred = textureSampleLevel(in2, pn_clamp, uv, 0.0).rgb;
            tap = mix(tap, blurred, smoothstep(0.0, 0.04, b));
        }
        mirrored += tap;
    }
    mirrored /= f32(TAPS);
    if textureDimensions(in3).x > 1u {
        // The things over the sky's reflection.
        let g = textureSampleLevel(in3, pn_clamp, v.pos.xy / pn.resolution, 0.0) * clamp(params.mirror, 0.0, 1.0);
        let lum = max(g.r, max(g.g, g.b));
        let rim = g.rgb * RIM_MIRROR * exp(-b * THING_FALL) / (1.0 + THING_KNEE * lum);
        mirrored = mirrored * (1.0 - g.a) + rim;
    }
    // Near the horizon the water mirrors most, looking down it shows little.
    let fresnel = mix(0.06, 1.0, exp(-b * 4.0));
    let strength = params.reflection * fresnel * mix(1.0, params.shore, shore_here);

    let water = mix(vec3f(0.0016, 0.0022, 0.0050), vec3f(0.0010, 0.0028, 0.0036), jade);
    var c = water + mirrored * strength + here.rgb * (1.0 - clamp(params.ground, 0.0, 1.0));

    // The mist: patches drifting on the water, thickest a few metres ahead, thicker with the bass,
    // thinner over open water.
    let near = smoothstep(34.0, 5.0, dist) * smoothstep(0.3, 1.6, dist);
    let n = 0.5 + 0.5 * pn_noise(vec3f(across * 0.16 + params.beats * 0.02, along * 0.11, params.beats * 0.012), 7u);
    let n2 = 0.5 + 0.5 * pn_noise(vec3f(across * 0.45, along * 0.33, params.beats * 0.03), 9u);
    let patches = smoothstep(0.30, 0.85, 0.7 * n + 0.3 * n2);
    let open = mix(0.4, 1.0, shore_here);
    let m = clamp(params.mist * (0.5 + 0.9 * clamp(params.energy, 0.0, 1.0)) * patches * near * open, 0.0, 1.0);
    let mist_c = mix(vec3f(0.004, 0.0065, 0.0058), vec3f(0.0035, 0.012, 0.0095), jade);
    c = c * (1.0 - 0.5 * m) + mist_c * m;

    // One pixel of soft edge at the horizon.
    let below = smoothstep(px, -px, at.y);
    return vec4f(mix(here.rgb, c, below), 1.0);
}
