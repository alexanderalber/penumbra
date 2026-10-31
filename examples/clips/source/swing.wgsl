// The clip of the clips example: 9 x 9 pendulums seen from above, each swinging out from the
// centre and back. All swing at one rate, once in 3.6 s. A pendulum's ring is its distance from
// the centre pendulum, rounded to whole grid steps. The rings fall into three groups (ring 0, 3
// and 6, ring 1 and 4, ring 2 and 5), a third of a swing apart. So the picture repeats exactly
// after one swing, and a clip of 3.6 s loops without a seam.
// Everything follows from the show time, so the picture renders the same every time.
// @param swing float = 0.34 0..0.5 -- how far a bob swings to either side, in grid steps
// @param glow float = 0.18 0.01..1 -- seconds a bob flashes after it passes its rest point
// @param colors palette = plasma -- the three groups' colours, picked from this palette
// @param ground color = [0.006, 0.008, 0.014, 1] -- the background

// One swing, in seconds. The lfos in project.ron swing at 1 / T.
const T: f32 = 3.6;
// The strikes fall 0.37 of a frame after a frame of the show clock, never on one: a crossing
// right on a frame lands on either side of it by rounding, and the loop would differ by a frame
// from one swing to the next. The lfos' phases carry the same lag.
const LAG: f32 = 0.37 / 60.0;
const TAU: f32 = 6.2831853;
// The layout, in pixels of a 1080 x 1080 picture; other sizes scale it to their height.
const REF: f32 = 1080.0;
const PITCH: f32 = 112.0;
const BOB: f32 = 13.0;
const HALF: f32 = 4.0;

// The ring's group, 0 to 2.
fn group(k: f32) -> f32 {
    return k - 3.0 * floor(k / 3.0);
}

// Swings of group g done at time t, wrapped to 0..1 (0: at rest, moving out). Group g is g / 3
// of a swing ahead, as the lfo with phase g / 3 - LAG / T.
fn swings(g: f32, t: f32) -> f32 {
    return fract((t - LAG) / T + g / 3.0);
}

// Seconds since group g last passed its rest point: it does twice per swing.
fn since(g: f32, t: f32) -> f32 {
    return fract(2.0 * swings(g, t)) * T / 2.0;
}

// How bright the flash of group g is: 1 right at the crossing, fading to 0 over `glow` seconds.
fn flash(g: f32, t: f32) -> f32 {
    let f = clamp(1.0 - since(g, t) / max(params.glow, 1e-3), 0.0, 1.0);
    return f * f;
}

fn ring(cell: vec2f) -> f32 {
    return round(length(cell));
}

fn heading(cell: vec2f) -> vec2f {
    if dot(cell, cell) < 0.5 {
        return vec2f(1.0, 0.0);
    }
    return normalize(cell);
}

fn hue(g: f32) -> vec3f {
    if params.colors.count == 0 {
        return vec3f(0.8);
    }
    return pn_colormap(params.colors, 0.3 + 0.32 * g).rgb;
}

fn segment(p: vec2f, a: vec2f, b: vec2f) -> f32 {
    let ab = b - a;
    let h = clamp(dot(p - a, ab) / max(dot(ab, ab), 1e-6), 0.0, 1.0);
    return length(p - a - ab * h);
}

// Coverage of a shape `r` wide around distance 0, antialiased over one output pixel `px`.
fn cover(d: f32, r: f32, px: f32) -> f32 {
    return clamp((r - d) / px + 0.5, 0.0, 1.0);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let scale = pn.resolution.y / REF;
    // Pixels of the 1080 x 1080 layout, with the centre pendulum at 0.
    let q = (v.pos.xy - pn.resolution * 0.5) / scale;
    let px = 1.0 / scale;
    let t = pn_time.seconds - T * floor(pn_time.seconds / T);
    let amp = params.swing * PITCH;

    var col = params.ground.rgb;
    // A faint circle through each ring lights up as the ring passes its rest point.
    let k_near = clamp(round(length(q) / PITCH), 1.0, 6.0);
    let circle = abs(length(q) - k_near * PITCH);
    let g_near = group(k_near);
    col += hue(g_near) * 0.25 * flash(g_near, t) * cover(circle, 1.5, px);

    let c0 = round(q / PITCH);
    // The tracks first, so no track crosses a bob of a neighbour.
    for (var dy = -1.0; dy <= 1.0; dy += 1.0) {
        for (var dx = -1.0; dx <= 1.0; dx += 1.0) {
            let cell = c0 + vec2f(dx, dy);
            if abs(cell.x) > HALF || abs(cell.y) > HALF {
                continue;
            }
            let rest = cell * PITCH;
            let h = heading(cell);
            let c = hue(group(ring(cell)));
            let track = cover(segment(q, rest - h * amp, rest + h * amp), 1.0, px);
            col = mix(col, c * 0.18, track);
            col = mix(col, c * 0.35, cover(length(q - rest), 2.5, px));
        }
    }
    for (var dy = -1.0; dy <= 1.0; dy += 1.0) {
        for (var dx = -1.0; dx <= 1.0; dx += 1.0) {
            let cell = c0 + vec2f(dx, dy);
            if abs(cell.x) > HALF || abs(cell.y) > HALF {
                continue;
            }
            let g = group(ring(cell));
            let bob = cell * PITCH + heading(cell) * amp * sin(TAU * swings(g, t));
            let f = flash(g, t);
            let d = length(q - bob);
            let c = hue(g);
            col += c * 0.6 * f * smoothstep(BOB + 22.0 * f, BOB, d);
            col = mix(col, mix(c * 0.8, mix(c, vec3f(1.0), 0.6), f), cover(d, BOB, px));
        }
    }
    return vec4f(col, 1.0);
}
