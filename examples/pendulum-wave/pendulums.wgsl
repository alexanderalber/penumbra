// A pendulum wave seen from above: 11 x 11 pendulums, each swinging out from the centre and
// back. A pendulum's ring is its distance from the centre pendulum, rounded to whole grid
// steps, 0 to 7. Ring k swings 56 - k times in 48 s, so the centre is fastest. All rings pass
// their rest point together at 0 s, drift apart and meet again at 24 s.
// Everything follows from the show time, so the picture loops and renders the same every time.
// @param swing float = 0.34 0..0.5 -- how far a bob swings to either side, in grid steps
// @param direction choice(radial|across) = radial -- radial: out from the centre and back; across: all left and right
// @param glow float = 0.15 0.01..1 -- seconds a bob flashes after it passes its rest point
// @param colors palette = plasma -- the rings' colours: the centre at the end of the palette, the corners near its start
// @param ground color = [0.006, 0.008, 0.014, 1] -- the background

const T: f32 = 48.0;
const N: f32 = 56.0;
const TAU: f32 = 6.2831853;
// The layout, in pixels of a 1080 x 1080 picture; other sizes scale it to their height.
const REF: f32 = 1080.0;
const PITCH: f32 = 90.0;
const BOB: f32 = 9.0;

// Swings of ring k done at time t, wrapped to 0..1 (0: at rest, moving out).
fn swings(k: f32, t: f32) -> f32 {
    return fract(t / T * (N - k));
}

// Seconds since ring k last passed its rest point: it does twice per swing.
fn since(k: f32, t: f32) -> f32 {
    return fract(2.0 * swings(k, t)) * T / (2.0 * (N - k));
}

// How bright the flash of ring k is: 1 right at the crossing, fading to 0 over `glow` seconds.
fn flash(k: f32, t: f32) -> f32 {
    let f = clamp(1.0 - since(k, t) / max(params.glow, 1e-3), 0.0, 1.0);
    return f * f;
}

// The pendulum's ring: its distance from the centre in grid steps, rounded.
fn ring(cell: vec2f) -> f32 {
    return round(length(cell));
}

fn heading(cell: vec2f) -> vec2f {
    if params.direction == direction_across || dot(cell, cell) < 0.5 {
        return vec2f(1.0, 0.0);
    }
    return normalize(cell);
}

fn hue(k: f32) -> vec3f {
    if params.colors.count == 0 {
        return vec3f(0.8);
    }
    // The start of most colormaps is too dark for the ground: the corners stop at 0.2.
    return pn_colormap(params.colors, 1.0 - 0.8 * k / 7.0).rgb;
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
    let r = length(q) / PITCH;
    let k_near = clamp(round(r), 1.0, 7.0);
    let circle = abs(length(q) - k_near * PITCH);
    col += hue(k_near) * 0.25 * flash(k_near, t) * cover(circle, 1.2, px);

    let c0 = round(q / PITCH);
    // The tracks first, so no track crosses a bob of a neighbour.
    for (var dy = -1.0; dy <= 1.0; dy += 1.0) {
        for (var dx = -1.0; dx <= 1.0; dx += 1.0) {
            let cell = c0 + vec2f(dx, dy);
            if abs(cell.x) > 5.0 || abs(cell.y) > 5.0 {
                continue;
            }
            let rest = cell * PITCH;
            let h = heading(cell);
            let k = ring(cell);
            let track = cover(segment(q, rest - h * amp, rest + h * amp), 0.8, px);
            col = mix(col, hue(k) * 0.18, track);
            col = mix(col, hue(k) * 0.35, cover(length(q - rest), 2.0, px));
        }
    }
    for (var dy = -1.0; dy <= 1.0; dy += 1.0) {
        for (var dx = -1.0; dx <= 1.0; dx += 1.0) {
            let cell = c0 + vec2f(dx, dy);
            if abs(cell.x) > 5.0 || abs(cell.y) > 5.0 {
                continue;
            }
            let k = ring(cell);
            let bob = cell * PITCH + heading(cell) * amp * sin(TAU * swings(k, t));
            let g = flash(k, t);
            let d = length(q - bob);
            let c = hue(k);
            col += c * 0.6 * g * smoothstep(BOB + 20.0 * g, BOB, d);
            col = mix(col, mix(c * 0.8, mix(c, vec3f(1.0), 0.6), g), cover(d, BOB, px));
        }
    }
    return vec4f(col, 1.0);
}
