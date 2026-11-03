// A huge human figure drawn in lines, standing on the horizon behind the floor, over black. It
// takes a few poses: standing, and on every second bar a sway with one arm out; through a build
// its arms rise; in a drop they are up, and the drop's flash makes it jump. It bobs on the
// beats. The lines glow in the stage's palette, warmer at the feet. Nothing below the horizon.
// Black where the sky is empty, so it can be screened over another sky.
// @param tall float = 0.42 0.05..1 -- the figure's height standing, in picture heights (arms up reach a fifth higher)
// @param place float = 0 -1.5..1.5 -- where it stands across the sky, in picture heights from straight ahead
// @param thickness float = 2.5 0.5..12 -- the lines' width, pixels
// @param glow float = 0.6 0..3 -- the light around the lines
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param bar float = 0 0..1 -- how far through the bar (bar of a chan.beat_grid)
// @param bar_index float = 0 0..100000 -- the bar's number (bar_index): the sway comes on every second bar
// @param build float = 0 0..1 -- rises over the bars before a boundary (build of a chan.beat_grid): the arms rise with it
// @param drop float = 0 0..1 -- the flash of a drop (drop of a chan.beat_grid): the figure jumps
// @param role float = 5 0..5 -- the section's role (section_role of a chan.beat_grid): in a drop, 3, the arms stay up
// @param energy float = 0 0..1 -- the bass (energy_low): the lines brighten with it
// @param warmth float = 0 0..1 -- 0 the warm palette, 1 the cold one, as the stage's
// @param brightness float = 1 0..3 -- the whole picture's light
// @param presence float = 1 0..1 -- how much of the figure is there: 1 all of it, 0 nothing (a dark sky)
// @param eye float = 1.5 0..10 -- the camera's height over the floor, metres (bind it to height of the stage's pose)
// @param pitch float = -2 -60..60 -- the camera's tilt up, degrees (pitch of the stage's pose)
// @param fov float = 60 10..120 -- the camera's vertical field of view, degrees (fov of the stage's pose)
// @param roll float = 0 -45..45 -- the camera's roll, degrees (roll of the stage's pose)
// @param side float = 0 -45..45 -- the camera's turn to the right, degrees (side of the stage's pose)
// @param reach float = 10 1..100 -- metres ahead where the floor fades out, the stage's `end`: the horizon lies there

// Where this pixel lies in the sky, in picture heights of a 60 degree view: x from straight ahead,
// y up from the horizon. The horizon is where the floor fades out, `reach` metres ahead, as the
// camera sees it: it moves with the camera's height, pitch, roll and turn, and things in the sky
// keep their size in degrees when the field of view changes. `.z`: one pixel in the same unit.
fn sky_at(px: vec2f) -> vec3f {
    let res = pn.resolution;
    let t = tan(radians(0.5 * clamp(params.fov, 1.0, 170.0)));
    // Tangents of the angle from the view's middle, y up.
    let q = vec2f((px.x - 0.5 * res.x) / res.y, 0.5 - px.y / res.y) * 2.0 * t;
    let r = radians(-params.roll);
    let p = vec2f(cos(r) * q.x - sin(r) * q.y, sin(r) * q.x + cos(r) * q.y);
    let down = atan(max(params.eye, 0.0) / max(params.reach, 0.1)) + radians(params.pitch);
    let unit = 2.0 * tan(radians(30.0));
    return vec3f(vec2f(p.x + tan(radians(params.side)), p.y + tan(down)) / unit, 2.0 * t / (res.y * unit));
}

// The palette: warm (0) runs yellow, orange, magenta; cold (1) cyan, blue, violet. `s` 0 to 1.
fn palette(s: f32) -> vec3f {
    let warm = mix(mix(vec3f(1.0, 0.86, 0.3), vec3f(1.0, 0.42, 0.18), smoothstep(0.0, 0.5, s)), vec3f(0.95, 0.12, 0.55), smoothstep(0.45, 1.0, s));
    let cold = mix(mix(vec3f(0.35, 0.95, 1.0), vec3f(0.25, 0.45, 1.0), smoothstep(0.0, 0.5, s)), vec3f(0.62, 0.18, 1.0), smoothstep(0.45, 1.0, s));
    return mix(warm, cold, clamp(params.warmth, 0.0, 1.0));
}

fn hash(a: u32, b: u32, c: u32) -> f32 {
    return f32(pn_hash(vec3u(a, b, c)).x) / 4294967296.0;
}

// The joints, in figure heights from the feet: 0 head, 1 neck, 2 pelvis, 3 4 shoulders, 5 6
// elbows, 7 8 hands, 9 10 hips, 11 12 knees, 13 14 feet (left first, as seen).
fn standing(j: u32) -> vec2f {
    switch j {
        case 0u: { return vec2f(0.0, 0.92); }
        case 1u: { return vec2f(0.0, 0.83); }
        case 2u: { return vec2f(0.0, 0.5); }
        case 3u: { return vec2f(-0.11, 0.8); }
        case 4u: { return vec2f(0.11, 0.8); }
        case 5u: { return vec2f(-0.14, 0.63); }
        case 6u: { return vec2f(0.14, 0.63); }
        case 7u: { return vec2f(-0.15, 0.46); }
        case 8u: { return vec2f(0.15, 0.46); }
        case 9u: { return vec2f(-0.07, 0.49); }
        case 10u: { return vec2f(0.07, 0.49); }
        case 11u: { return vec2f(-0.08, 0.26); }
        case 12u: { return vec2f(0.08, 0.26); }
        case 13u: { return vec2f(-0.09, 0.0); }
        default: { return vec2f(0.09, 0.0); }
    }
}

// Weight on the left leg, the right arm out and up, the head tilted.
fn sway(j: u32) -> vec2f {
    switch j {
        case 0u: { return vec2f(0.035, 0.915); }
        case 1u: { return vec2f(0.02, 0.83); }
        case 2u: { return vec2f(-0.02, 0.5); }
        case 3u: { return vec2f(-0.09, 0.79); }
        case 4u: { return vec2f(0.13, 0.81); }
        case 5u: { return vec2f(-0.15, 0.63); }
        case 6u: { return vec2f(0.26, 0.86); }
        case 7u: { return vec2f(-0.12, 0.47); }
        case 8u: { return vec2f(0.33, 1.0); }
        case 9u: { return vec2f(-0.09, 0.49); }
        case 10u: { return vec2f(0.05, 0.47); }
        case 11u: { return vec2f(-0.09, 0.26); }
        case 12u: { return vec2f(0.12, 0.24); }
        case 13u: { return vec2f(-0.09, 0.0); }
        default: { return vec2f(0.1, 0.02); }
    }
}

// Arms out level, reaching up a little.
fn raising(j: u32) -> vec2f {
    switch j {
        case 5u: { return vec2f(-0.27, 0.8); }
        case 6u: { return vec2f(0.27, 0.8); }
        case 7u: { return vec2f(-0.43, 0.86); }
        case 8u: { return vec2f(0.43, 0.86); }
        default: { return standing(j); }
    }
}

// Arms up in a V, legs apart.
fn arms_up(j: u32) -> vec2f {
    switch j {
        case 0u: { return vec2f(0.0, 0.93); }
        case 5u: { return vec2f(-0.2, 0.99); }
        case 6u: { return vec2f(0.2, 0.99); }
        case 7u: { return vec2f(-0.3, 1.18); }
        case 8u: { return vec2f(0.3, 1.18); }
        case 11u: { return vec2f(-0.13, 0.27); }
        case 12u: { return vec2f(0.13, 0.27); }
        case 13u: { return vec2f(-0.19, 0.0); }
        case 14u: { return vec2f(0.19, 0.0); }
        default: { return standing(j); }
    }
}

// The bones, as pairs of joints.
fn bone(k: u32) -> vec2u {
    switch k {
        case 0u: { return vec2u(1u, 2u); }
        case 1u: { return vec2u(3u, 4u); }
        case 2u: { return vec2u(3u, 5u); }
        case 3u: { return vec2u(5u, 7u); }
        case 4u: { return vec2u(4u, 6u); }
        case 5u: { return vec2u(6u, 8u); }
        case 6u: { return vec2u(9u, 10u); }
        case 7u: { return vec2u(9u, 11u); }
        case 8u: { return vec2u(11u, 13u); }
        case 9u: { return vec2u(10u, 12u); }
        default: { return vec2u(12u, 14u); }
    }
}

fn segment(p: vec2f, a: vec2f, b: vec2f) -> f32 {
    let ab = b - a;
    let t = clamp(dot(p - a, ab) / max(dot(ab, ab), 1e-9), 0.0, 1.0);
    return length(p - a - ab * t);
}

struct Blend {
    sway: f32,
    raise: f32,
    up: f32,
    lift: f32,
};

fn joint(j: u32, w: Blend) -> vec2f {
    var p = mix(standing(j), sway(j), w.sway);
    p = mix(p, raising(j), w.raise);
    p = mix(p, arms_up(j), w.up);
    // The body bobs down by bending the knees; the feet stay.
    if j < 11u {
        p.y -= w.lift;
    } else if j < 13u {
        p.y -= 0.5 * w.lift;
    }
    return p;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let at = sky_at(v.pos.xy);
    let tall = max(params.tall, 0.01);
    // The pose: a sway every second bar, eased over the bar's first quarter.
    let odd = f32(i32(floor(params.bar_index)) & 1);
    let ease = smoothstep(0.0, 0.25, clamp(params.bar, 0.0, 1.0));
    var w: Blend;
    w.sway = mix(1.0 - odd, odd, ease);
    let build = clamp(params.build, 0.0, 1.0);
    let in_drop = 1.0 - smoothstep(0.0, 0.5, abs(params.role - 3.0));
    w.raise = smoothstep(0.0, 0.5, build) * (1.0 - in_drop);
    w.up = max(smoothstep(0.5, 1.0, build) * (1.0 - in_drop), in_drop);
    w.sway *= (1.0 - w.raise) * (1.0 - w.up);
    // A bob on each beat, deeper in a drop; the drop's flash throws the whole figure up.
    let beat = fract(params.beats);
    w.lift = (0.008 + 0.012 * in_drop) * (0.5 - 0.5 * cos(6.2831853 * beat));
    let jump = 0.12 * clamp(params.drop, 0.0, 1.0);
    let p = (at.xy - vec2f(params.place, 0.0)) / tall - vec2f(0.0, jump);
    let px = at.z / tall;
    var d = 1e9;
    for (var k = 0u; k < 11u; k++) {
        let b = bone(k);
        d = min(d, segment(p, joint(b.x, w), joint(b.y, w)));
    }
    // The head: a ring.
    let head = joint(0u, w);
    d = min(d, abs(length(p - head) - 0.075));
    let half = 0.5 * max(params.thickness, 0.5) * px;
    let core = 1.0 - smoothstep(half - px, half + px, d);
    let halo = params.glow * 0.35 * exp(-max(d - half, 0.0) / (10.0 * px));
    let energy = clamp(params.energy, 0.0, 1.0);
    let colour = palette(clamp(1.0 - p.y, 0.0, 1.0));
    var light = colour * (core * 1.2 + halo) * (0.75 + 0.4 * energy);
    light *= smoothstep(-at.z, at.z, at.y);
    return vec4f(light * params.brightness * clamp(params.presence, 0.0, 1.0), 1.0);
}
