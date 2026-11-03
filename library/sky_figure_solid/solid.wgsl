// A huge human figure standing on the horizon, a filled low-poly silhouette over black: each limb
// and the trunk a few flat facets of dark fill, lit from the upper left, inside a bright edge in the
// stage's palette. It takes the poses of sky_figure: standing, a sway with one arm out on every
// second bar, arms rising through a build, up in a drop, a jump on the drop's flash, a bob on each
// beat. Black where the sky is empty, so it can be screened over another sky.
// @param tall float = 0.42 0.05..1.5 -- its height standing, in picture heights
// @param tube float = 0.3 0.05..1 -- its height in a tube's far end (with `bend`), in picture heights
// @param thickness float = 2 0.5..12 -- the bright edge's width, pixels
// @param fill float = 0.3 0..1 -- the facets' light, against the edge's
// @param seams float = 0.35 0..1 -- the light of the lines between facets
// @param glow float = 0.6 0..3 -- the light around the edge
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

// Signed distance to a trapezoid round the segment a to b, half width ra at a and rb at b.
fn sd_trapezoid(p: vec2f, a: vec2f, b: vec2f, ra: f32, rb: f32) -> f32 {
    let rba = rb - ra;
    let baba = max(dot(b - a, b - a), 1e-8);
    let papa = dot(p - a, p - a);
    let paba = dot(p - a, b - a) / baba;
    let x = sqrt(max(papa - paba * paba * baba, 0.0));
    let cax = max(0.0, x - select(rb, ra, paba < 0.5));
    let cay = abs(paba - 0.5) - 0.5;
    let k = rba * rba + baba;
    let f = clamp((rba * (x - ra) + paba * baba) / k, 0.0, 1.0);
    let cbx = x - ra - f * rba;
    let cby = paba - f;
    let s = select(1.0, -1.0, cbx < 0.0 && cay < 0.0);
    return s * sqrt(min(cax * cax + cay * cay * baba, cbx * cbx + cby * cby * baba));
}

// Signed distance to a hexagon of inner radius r, corners left and right.
fn sd_hex(p: vec2f, r: f32) -> f32 {
    let k = vec3f(-0.866025404, 0.5, 0.577350269);
    var q = abs(p.yx);
    q -= 2.0 * min(dot(k.xy, q), 0.0) * k.xy;
    q -= vec2f(clamp(q.x, -k.z * r, k.z * r), r);
    return length(q) * sign(q.y);
}

// Signed distance to a convex quad given counter-clockwise.
fn sd_quad(p: vec2f, v0: vec2f, v1: vec2f, v2: vec2f, v3: vec2f) -> f32 {
    var v = array<vec2f, 4>(v0, v1, v2, v3);
    var d = dot(p - v0, p - v0);
    var s = 1.0;
    var j = 3u;
    for (var i = 0u; i < 4u; i++) {
        let e = v[j] - v[i];
        let w = p - v[i];
        let b = w - e * clamp(dot(w, e) / max(dot(e, e), 1e-9), 0.0, 1.0);
        d = min(d, dot(b, b));
        let c1 = p.y >= v[i].y;
        let c2 = p.y < v[j].y;
        let c3 = e.x * w.y > e.y * w.x;
        if (c1 && c2 && c3) || (!c1 && !c2 && !c3) {
            s = -s;
        }
        j = i;
    }
    return s * sqrt(d);
}

// The nearest piece so far: its distance, the light of its facet under p, the distance to its seams.
struct Hit {
    d: f32,
    shade: f32,
    seam: f32,
};

fn take(h: Hit, d: f32, shade: f32, seam: f32) -> Hit {
    if d < h.d {
        return Hit(d, shade, seam);
    }
    return h;
}

const LIGHT: vec2f = vec2f(-0.6, 0.8);

// A facet facing `n` (in the picture's plane, outwards), lit from the upper left.
fn facet(n: vec2f, k: u32) -> f32 {
    return 0.45 + 0.4 * dot(normalize(n), LIGHT) + 0.15 * hash(k, 7u, 1u);
}

// A limb: a trapezoid split along its middle into two facets.
fn limb(h: Hit, p: vec2f, a: vec2f, b: vec2f, ra: f32, rb: f32, k: u32) -> Hit {
    let d = sd_trapezoid(p, a, b, ra, rb);
    let e = b - a;
    let across = vec2f(-e.y, e.x);
    let side = select(-1.0, 1.0, dot(p - a, across) > 0.0);
    return take(h, d, facet(across * side, k), segment(p, a, b));
}

// A hexagon of six facets round its middle, turned by `angle`.
fn hexagon(h: Hit, p: vec2f, c: vec2f, r: f32, angle: f32, k: u32) -> Hit {
    let cs = cos(angle);
    let sn = sin(angle);
    let q = vec2f(cs * (p.x - c.x) + sn * (p.y - c.y), -sn * (p.x - c.x) + cs * (p.y - c.y));
    let d = sd_hex(q, r);
    let sector = 1.0471976;
    let a = atan2(q.y, q.x);
    let mid = (floor(a / sector) + 0.5) * sector + angle;
    // The spokes from the middle to the corners.
    let off = a - round(a / sector) * sector;
    let seam = length(q) * abs(sin(off));
    return take(h, d, facet(vec2f(cos(mid), sin(mid)), k + u32(floor(a / sector) + 3.0)), seam);
}

// The trunk: four facets fanned from its middle, one per side of the quad.
fn trunk(h: Hit, p: vec2f, v0: vec2f, v1: vec2f, v2: vec2f, v3: vec2f) -> Hit {
    let d = sd_quad(p, v0, v1, v2, v3);
    let c = 0.25 * (v0 + v1 + v2 + v3);
    var v = array<vec2f, 4>(v0, v1, v2, v3);
    var shade = 0.5;
    var seam = 1e9;
    for (var i = 0u; i < 4u; i++) {
        let a = v[i] - c;
        let b = v[(i + 1u) % 4u] - c;
        let q = p - c;
        if a.x * q.y - a.y * q.x >= 0.0 && q.x * b.y - q.y * b.x >= 0.0 {
            let e = b - a;
            shade = facet(vec2f(e.y, -e.x), 40u + i);
        }
        seam = min(seam, segment(p, c, v[i]));
    }
    return take(h, d, shade, seam);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let at = sky_at(v.pos.xy);
    let g = ground();
    let o = pose_now(false);
    let w = o.w;
    let p = (at.xy - g.foot) / g.tall - vec2f(0.0, o.jump);
    let px = at.z / g.tall;
    var h = Hit(1e9, 0.0, 1e9);
    let neck = joint(1u, w);
    let hip_l = joint(9u, w);
    let hip_r = joint(10u, w);
    h = trunk(h, p, joint(3u, w) + vec2f(-0.02, 0.01), hip_l + vec2f(-0.015, -0.03), hip_r + vec2f(0.015, -0.03), joint(4u, w) + vec2f(0.02, 0.01));
    h = limb(h, p, neck + vec2f(0.0, -0.02), joint(0u, w), 0.03, 0.026, 1u);
    // Legs, feet pointing outwards.
    h = limb(h, p, hip_l, joint(11u, w), 0.048, 0.034, 2u);
    h = limb(h, p, joint(11u, w), joint(13u, w), 0.034, 0.022, 3u);
    h = limb(h, p, hip_r, joint(12u, w), 0.048, 0.034, 4u);
    h = limb(h, p, joint(12u, w), joint(14u, w), 0.034, 0.022, 5u);
    h = limb(h, p, joint(13u, w) + vec2f(0.01, 0.0), joint(13u, w) + vec2f(-0.045, -0.004), 0.02, 0.01, 6u);
    h = limb(h, p, joint(14u, w) + vec2f(-0.01, 0.0), joint(14u, w) + vec2f(0.045, -0.004), 0.02, 0.01, 7u);
    // Arms and hands.
    h = limb(h, p, joint(3u, w), joint(5u, w), 0.036, 0.027, 8u);
    h = limb(h, p, joint(5u, w), joint(7u, w), 0.027, 0.019, 9u);
    h = limb(h, p, joint(4u, w), joint(6u, w), 0.036, 0.027, 10u);
    h = limb(h, p, joint(6u, w), joint(8u, w), 0.027, 0.019, 11u);
    h = hexagon(h, p, joint(7u, w), 0.022, 0.3, 12u);
    h = hexagon(h, p, joint(8u, w), 0.022, -0.3, 20u);
    // The head, tilted with the neck.
    let hd = joint(0u, w);
    let tilt = atan2(-(hd.x - neck.x), hd.y - neck.y);
    h = hexagon(h, p, hd + vec2f(0.0, 0.01), 0.07, tilt + 0.5236, 28u);

    let energy = clamp(params.energy, 0.0, 1.0);
    let colour = palette(clamp(1.0 - p.y, 0.0, 1.0));
    let inside = 1.0 - smoothstep(-px, px, h.d);
    let half = 0.5 * max(params.thickness, 0.5) * px;
    let edge = 1.0 - smoothstep(half - px, half + px, abs(h.d));
    let seam = (1.0 - smoothstep(0.4 * px, 1.4 * px, h.seam)) * inside;
    let halo = params.glow * 0.3 * exp(-max(h.d - half, 0.0) / (10.0 * px)) * (1.0 - inside);
    var light = colour * (inside * params.fill * h.shade * (1.0 - edge) + seam * params.seams + edge * 1.2 + halo);
    light *= 0.75 + 0.4 * energy;
    return finish(light, at, g);
}
