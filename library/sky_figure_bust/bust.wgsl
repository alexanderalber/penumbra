// A huge bust rising over the horizon: head, neck and shoulders as a wireframe of lines of latitude
// and longitude round their bodies, a bright rim where a body turns away, two glowing eyes and a
// mouth. Its poses follow sky_figure's: on every second bar the head tilts to one side; through a
// build the shoulders rise and the head lifts; in a drop the head is tilted far back, looking up,
// and the drop's flash makes the whole bust jump; it nods on each beat. Black where the sky is
// empty, so it can be screened over another sky.
// @param tall float = 0.55 0.05..1.5 -- its height standing, in picture heights
// @param tube float = 0.28 0.05..1 -- its height in a tube's far end (with `bend`), in picture heights
// @param thickness float = 1.4 0.5..12 -- the lines' width, pixels
// @param rim float = 1 0..3 -- the light of the rim
// @param fill float = 0.06 0..0.5 -- the bodies' faint light between the lines
// @param eyes float = 1.2 0..3 -- the eyes' light
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

// Local to picture: turned about x by `back` (the top away from the viewer), then about z by `tilt`.
fn turn(v: vec3f, back: f32, tilt: f32) -> vec3f {
    let cb = cos(back);
    let sb = sin(back);
    let a = vec3f(v.x, cb * v.y + sb * v.z, -sb * v.y + cb * v.z);
    let ct = cos(tilt);
    let st = sin(tilt);
    return vec3f(ct * a.x - st * a.y, st * a.x + ct * a.y, a.z);
}

fn unturn(v: vec3f, back: f32, tilt: f32) -> vec3f {
    let ct = cos(tilt);
    let st = sin(tilt);
    let a = vec3f(ct * v.x + st * v.y, -st * v.x + ct * v.y, v.z);
    let cb = cos(back);
    let sb = sin(back);
    return vec3f(a.x, cb * a.y - sb * a.z, sb * a.y + cb * a.z);
}

// Where a viewer's ray at q hits a body: its nearness, the point on the unit sphere (or cylinder)
// in the body's own frame, how much the surface faces the viewer, and the body's size.
struct Surf {
    z: f32,
    s: vec3f,
    facing: f32,
    size: f32,
};

const MISS: f32 = -1e9;

fn ellipsoid(q: vec2f, c: vec3f, r: vec3f, back: f32, tilt: f32) -> Surf {
    var hit = Surf(MISS, vec3f(0.0), 0.0, r.y);
    let o = unturn(vec3f(q - c.xy, 10.0), back, tilt) / r;
    let dl = unturn(vec3f(0.0, 0.0, -1.0), back, tilt) / r;
    let a = dot(dl, dl);
    let b = dot(o, dl);
    let disc = b * b - a * (dot(o, o) - 1.0);
    if disc <= 0.0 {
        return hit;
    }
    let t = (-b - sqrt(disc)) / a;
    hit.s = o + dl * t;
    hit.z = c.z + 10.0 - t;
    hit.facing = clamp(normalize(turn(hit.s / r, back, tilt)).z, 0.0, 1.0);
    return hit;
}

// An upright cylinder of radius r round x = 0, from y0 to y1.
fn cylinder(q: vec2f, r: f32, y0: f32, y1: f32) -> Surf {
    var hit = Surf(MISS, vec3f(0.0), 0.0, r);
    if abs(q.x) >= r || q.y < y0 || q.y > y1 {
        return hit;
    }
    let z = sqrt(r * r - q.x * q.x);
    hit.s = vec3f(q.x / r, (q.y - y0) / r, z / r);
    hit.z = z;
    hit.facing = z / r;
    return hit;
}

fn nearer(a: Surf, b: Surf) -> Surf {
    if b.z > a.z {
        return b;
    }
    return a;
}

// Lines of latitude and longitude on a unit sphere point, `px` one pixel in units of its radius.
fn grid(s: vec3f, facing: f32, px: f32, wide: f32, lons: f32, lats: f32) -> f32 {
    let lon = atan2(s.x, s.z);
    let lat = asin(clamp(s.y, -1.0, 1.0));
    let ls = 6.2831853 / lons;
    let ts = 3.14159265 / lats;
    let blur = px / max(facing, 0.2);
    let dl = abs(fract(lon / ls + 0.5) - 0.5) * ls * cos(lat);
    let dt = abs(fract(lat / ts + 0.5) - 0.5) * ts;
    let a = 1.0 - smoothstep(wide * blur, (wide + 1.0) * blur, dl);
    let b = 1.0 - smoothstep(wide * blur, (wide + 1.0) * blur, dt);
    // Where the lines crowd near the rim, they fade into it.
    return max(a, b) * smoothstep(0.05, 0.3, facing);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let at = sky_at(v.pos.xy);
    let g = ground();
    let o = pose_now(false);
    let w = o.w;
    let q = (at.xy - g.foot) / g.tall - vec2f(0.0, 0.8 * o.jump);
    let px = at.z / g.tall;
    let wide = 0.5 * max(params.thickness, 0.5);
    // The pose: a tilt to the side on the sway, the shoulders up through a build, the head back in
    // a drop, a nod on each beat.
    let beat = fract(params.beats);
    let nod = radians(4.0) * (1.0 + o.in_drop) * (0.5 - 0.5 * cos(6.2831853 * beat));
    let back = radians(10.0) * w.raise + radians(38.0) * w.up - nod;
    let tilt = radians(-13.0) * w.sway;
    let shrug = 0.035 * w.raise + 0.05 * w.up - 2.0 * w.lift;
    let lean = radians(-4.0) * w.sway;
    let neck_top = vec2f(-0.03 * w.sway, 0.5 + shrug);
    let hc = vec3f(neck_top, 0.0) + turn(vec3f(0.0, 0.23, 0.03), back, tilt);
    let head = ellipsoid(q, hc, vec3f(0.19, 0.25, 0.22), back, tilt);
    let neck = cylinder(q - vec2f(neck_top.x, 0.0), 0.085, 0.25, neck_top.y + 0.06);
    let body = ellipsoid(q, vec3f(0.0, shrug, -0.05), vec3f(0.95, 0.36, 0.4), 0.0, lean);
    let s = nearer(nearer(body, neck), head);
    var light = 0.0;
    var spots = 0.0;
    if s.z > MISS * 0.5 {
        let lp = px / s.size;
        var lines = 0.0;
        if s.z == head.z {
            lines = grid(s.s, s.facing, lp, wide, 18.0, 10.0);
            // The face, on the front: eyes and a mouth.
            let e = 0.36;
            let eye_l = normalize(vec3f(-e, 0.16, 0.92));
            let eye_r = normalize(vec3f(e, 0.16, 0.92));
            let de = min(acos(clamp(dot(s.s, eye_l), -1.0, 1.0)), acos(clamp(dot(s.s, eye_r), -1.0, 1.0)));
            spots = (1.0 - smoothstep(0.09, 0.09 + 2.0 * lp, de)) + 0.6 * exp(-de / 0.08);
            let lon = atan2(s.s.x, s.s.z);
            let lat = asin(clamp(s.s.y, -1.0, 1.0));
            let mouth = abs(lat + 0.42 - 0.3 * lon * lon) * step(abs(lon), 0.3);
            spots += (1.0 - smoothstep(wide * lp, (wide + 1.0) * lp, mouth)) * step(abs(lon), 0.3) * 0.8;
        } else if s.z == neck.z {
            let ang = asin(clamp(s.s.x, -1.0, 1.0));
            let ls = 6.2831853 / 14.0;
            let blur = lp / max(s.facing, 0.2);
            let dl = abs(fract(ang / ls + 0.5) - 0.5) * ls;
            let dy = abs(fract(s.s.y / 0.9 + 0.5) - 0.5) * 0.9;
            lines = max(1.0 - smoothstep(wide * blur, (wide + 1.0) * blur, dl), 1.0 - smoothstep(wide * lp, (wide + 1.0) * lp, dy));
            lines *= smoothstep(0.05, 0.3, s.facing);
        } else {
            lines = grid(s.s, s.facing, lp, wide, 28.0, 8.0);
        }
        let rim = (1.0 - smoothstep(0.0, 0.22, s.facing)) * params.rim;
        light = max(lines, rim) + params.fill;
    }
    let energy = clamp(params.energy, 0.0, 1.0);
    let colour = palette(clamp(1.0 - q.y, 0.0, 1.0));
    let accent = mix(palette(0.95), vec3f(1.0), 0.35);
    var res = colour * light * 1.1 + accent * spots * params.eyes * (0.8 + 0.5 * energy);
    res *= 0.75 + 0.4 * energy;
    return finish(res, at, g);
}
