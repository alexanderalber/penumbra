// A huge dancer standing on the horizon: a slim body of lines with a filled head, and a cloak of
// thin lines hanging from the arms. The lines fan from the shoulders, along the arms, to the wrists
// and fall from there; they swing with the mids, the outer ones a little behind the inner ones, so
// the cloak trails the arms. With the arms up the cloak opens like wings. It takes the poses of
// sky_figure: standing, a sway with one arm out on every second bar, arms rising through a build,
// up in a drop, a jump on the drop's flash, a bob on each beat. Black where the sky is empty, so it
// can be screened over another sky.
// @param tall float = 0.42 0.05..1.5 -- its height standing, in picture heights
// @param tube float = 0.3 0.05..1 -- its height in a tube's far end (with `bend`), in picture heights
// @param thickness float = 1.8 0.5..12 -- the body's lines, pixels (the cloak's are half as wide)
// @param strands float = 12 2..24 -- lines in the cloak on each side
// @param cloak float = 0.55 0..2 -- the cloak's light
// @param hem float = 0.3 0..0.8 -- the cloak's lower edge, in figure heights from the feet
// @param swing float = 0.06 0..0.3 -- how far the cloak swings in quiet passages, in figure heights
// @param mid float = 0 0..1 -- the mids (energy_mid): the cloak swings wider with them
// @param glow float = 0.6 0..3 -- the light around the lines
// @param place float = 0 -1.5..1.5 -- where it stands across the sky, in picture heights from straight ahead
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param bar float = 0 0..1 -- how far through the bar (bar of a chan.beat_grid)
// @param bar_index float = 0 0..100000 -- the bar's number (bar_index): the sway comes on every second bar
// @param build float = 0 0..1 -- rises over the bars before a boundary (build of a chan.beat_grid): the arms rise with it
// @param drop float = 0 0..1 -- the flash of a drop (drop of a chan.beat_grid): the figure jumps
// @param role float = 5 0..5 -- the section's role (section_role of a chan.beat_grid): in a drop, 3, the arms stay up
// @param energy float = 0 0..1 -- the bass (energy_low): the light rises with it
// @param warmth float = 0 0..1 -- 0 the warm palette, 1 the cold one, as the stage's
// @param brightness float = 1 0..3 -- the whole picture's light
// @param presence float = 1 0..1 -- how much of the figure is there: 1 all of it, 0 nothing (a dark sky)
// @param eye float = 1.5 0..10 -- the camera's height over the floor, metres (bind it to height of the stage's pose)
// @param pitch float = -2 -60..60 -- the camera's tilt up, degrees (pitch of the stage's pose)
// @param fov float = 60 10..120 -- the camera's vertical field of view, degrees (fov of the stage's pose)
// @param roll float = 0 -45..45 -- the camera's roll, degrees (roll of the stage's pose)
// @param side float = 0 -45..45 -- the camera's turn to the right, degrees (side of the stage's pose)
// @param reach float = 10 1..100 -- metres ahead where the floor fades out, the stage's `end`: the horizon lies there
// @param bend float = 0 0..1 -- 0 it stands on the horizon; 1 it stands in a tube's far end, on the bottom of a ring of `ring` round the vanishing point (curve of the stage's pose)
// @param ring float = 0.17 0.02..1 -- the far end's radius at full bend, in picture heights of a 60 degree view (0.17: stage_tunnel's tube)

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

// Where the figure's feet stand and how tall it is: on the horizon at `place`, or with `bend`
// on the bottom of the tube's far end, straight ahead, at the height `tube`.
struct Ground {
    foot: vec2f,
    tall: f32,
};

fn ground() -> Ground {
    let b = clamp(params.bend, 0.0, 1.0);
    let unit = 2.0 * tan(radians(30.0));
    let down = atan(max(params.eye, 0.0) / max(params.reach, 0.1)) + radians(params.pitch);
    // The vanishing point's height above the horizon.
    let vanish = (tan(down) - tan(radians(params.pitch))) / unit;
    var g: Ground;
    g.foot = vec2f(params.place * (1.0 - b), b * (vanish - max(params.ring, 0.02)));
    g.tall = mix(max(params.tall, 0.01), max(params.tube, 0.01), b);
    return g;
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

fn segment(p: vec2f, a: vec2f, b: vec2f) -> f32 {
    let ab = b - a;
    let t = clamp(dot(p - a, ab) / max(dot(ab, ab), 1e-9), 0.0, 1.0);
    return length(p - a - ab * t);
}

// How far along a to b the point of the segment nearest p lies, 0 to 1.
fn along(p: vec2f, a: vec2f, b: vec2f) -> f32 {
    let ab = b - a;
    return clamp(dot(p - a, ab) / max(dot(ab, ab), 1e-9), 0.0, 1.0);
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

struct Pose {
    w: Blend,
    jump: f32,
    in_drop: f32,
};

// The pose now: a sway every second bar, eased over the bar's first quarter; through a build the
// arms rise, in a drop they are up; a bob on each beat, deeper in a drop; the drop's flash throws
// the whole figure up. `steps` true: the same poses, taken in steps (the arms in quarters, the
// sway at once on the bar, the bob as a drop on the second half of each beat).
fn pose_now(steps: bool) -> Pose {
    var o: Pose;
    let odd = f32(i32(floor(params.bar_index)) & 1);
    var ease = smoothstep(0.0, 0.25, clamp(params.bar, 0.0, 1.0));
    var build = clamp(params.build, 0.0, 1.0);
    if steps {
        ease = 1.0;
        build = floor(build * 4.0 + 0.5) / 4.0;
    }
    o.w.sway = mix(1.0 - odd, odd, ease);
    o.in_drop = 1.0 - smoothstep(0.0, 0.5, abs(params.role - 3.0));
    o.w.raise = smoothstep(0.0, 0.5, build) * (1.0 - o.in_drop);
    o.w.up = max(smoothstep(0.5, 1.0, build) * (1.0 - o.in_drop), o.in_drop);
    o.w.sway *= (1.0 - o.w.raise) * (1.0 - o.w.up);
    let beat = fract(params.beats);
    let deep = 0.008 + 0.012 * o.in_drop;
    if steps {
        o.w.lift = 1.4 * deep * step(0.5, beat);
    } else {
        o.w.lift = deep * (0.5 - 0.5 * cos(6.2831853 * beat));
    }
    o.jump = 0.12 * clamp(params.drop, 0.0, 1.0);
    return o;
}

// The light's last steps: nothing below the feet' line (the floor stands there), brightness and
// presence.
fn finish(light: vec3f, at: vec3f, g: Ground) -> vec4f {
    let cut = smoothstep(-at.z, at.z, at.y - g.foot.y);
    return vec4f(light * cut * params.brightness * clamp(params.presence, 0.0, 1.0), 1.0);
}

// A point along an arm, from the shoulder (0) through the elbow to the wrist (1).
fn arm_at(s: f32, a: vec2f, b: vec2f, c: vec2f) -> vec2f {
    if s < 0.5 {
        return mix(a, b, 2.0 * s);
    }
    return mix(b, c, 2.0 * s - 1.0);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let at = sky_at(v.pos.xy);
    let g = ground();
    let o = pose_now(false);
    let w = o.w;
    let p = (at.xy - g.foot) / g.tall - vec2f(0.0, o.jump);
    let px = at.z / g.tall;
    // The body: thin bones and a filled head.
    var d = 1e9;
    d = min(d, segment(p, joint(1u, w), joint(2u, w)));
    d = min(d, segment(p, joint(3u, w), joint(4u, w)));
    d = min(d, segment(p, joint(3u, w), joint(5u, w)));
    d = min(d, segment(p, joint(5u, w), joint(7u, w)));
    d = min(d, segment(p, joint(4u, w), joint(6u, w)));
    d = min(d, segment(p, joint(6u, w), joint(8u, w)));
    d = min(d, segment(p, joint(9u, w), joint(10u, w)));
    d = min(d, segment(p, joint(9u, w), joint(11u, w)));
    d = min(d, segment(p, joint(11u, w), joint(13u, w)));
    d = min(d, segment(p, joint(10u, w), joint(12u, w)));
    d = min(d, segment(p, joint(12u, w), joint(14u, w)));
    d = min(d, segment(p, joint(1u, w), joint(0u, w) - vec2f(0.0, 0.035)));
    d = min(d, max(length(p - joint(0u, w)) - 0.04, 0.0));
    // The cloak: per side, strands from points along the arm down to the hem, each bent in the
    // middle and swinging with the mids, the outer ones later.
    let mids = clamp(params.mid, 0.0, 1.0);
    let amp = params.swing * (1.0 + 2.5 * mids);
    let n = clamp(i32(params.strands), 2, 24);
    var c = 1e9;
    var shade = 0.0;
    for (var k = 0u; k < 2u; k++) {
        let outward = select(1.0, -1.0, k == 0u);
        let a = joint(3u + k, w);
        let b = joint(5u + k, w);
        let e = joint(7u + k, w);
        for (var i = 0; i < n; i++) {
            let s = f32(i) / f32(n - 1);
            let top = arm_at(s, a, b, e);
            let fall = max(top.y - mix(params.hem, params.hem + 0.08, s), 0.06);
            let phase = 6.2831853 * (0.5 * params.beats) - 2.2 * s + 3.14159 * f32(k);
            let drift = amp * sin(phase) * (0.3 + s);
            let bottom = top + vec2f(outward * (0.04 + 0.14 * s) + drift, -fall);
            let middle = mix(top, bottom, 0.5) + vec2f(0.5 * amp * sin(phase - 1.2) + outward * 0.03 * s, 0.0);
            let dd = min(segment(p, top, middle), segment(p, middle, bottom));
            if dd < c {
                c = dd;
                shade = s;
            }
        }
    }
    let half = 0.5 * max(params.thickness, 0.5) * px;
    let core = 1.0 - smoothstep(half - px, half + px, d);
    let thin = 1.0 - smoothstep(0.5 * half - px, 0.5 * half + px, c);
    let halo = params.glow * 0.35 * exp(-max(d - half, 0.0) / (10.0 * px)) + params.glow * 0.12 * exp(-max(c, 0.0) / (6.0 * px));
    let energy = clamp(params.energy, 0.0, 1.0);
    let colour = palette(clamp(1.0 - p.y, 0.0, 1.0));
    let cape = palette(clamp(0.35 + 0.6 * shade, 0.0, 1.0));
    var light = (colour * (core * 1.2) + cape * thin * params.cloak * (0.8 + 0.6 * mids) + colour * halo) * (0.75 + 0.4 * energy);
    return finish(light, at, g);
}
