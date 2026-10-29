// The pendulum wave: 10 x 10 pendulums in 10 rings, the centre ring fastest. Ring k swings
// 80 - k times in 64 s. All rings pass the bottom together at 0 s, drift apart and meet again
// at 32 s. Below the grid: the sync R(t) over the loop, and a phase circle with a dot per ring.
// Everything follows from the show time, so the picture loops and renders the same every time.
// in0: the labels, laid out on a 1080 x 1102 picture; their alpha is the ink.
// @param look choice(fade|light|dark) = fade -- fade: light ground for 8 s, dark from 12 s, light again over the loop's last 4 s
// @param swing float = 24 0..60 -- degrees each pendulum swings to either side
// @param glow float = 0.12 0.01..1 -- seconds a bob glows after it passes the bottom
// @param glow_color color = [1.0, 0.33, 0.05, 1] -- the halo around a bob that passes the bottom

const T: f32 = 64.0;
const TAU: f32 = 6.2831853;
// The layout, in pixels of a 1080 x 1102 picture; other sizes scale it to their height.
const REF: vec2f = vec2f(1080.0, 1102.0);
const ORIGIN: vec2f = vec2f(170.0, 22.0);
const PITCH: vec2f = vec2f(82.2, 82.0);
const ROD: f32 = 58.0;
const BOB: f32 = 11.5;
const PLOT_X0: f32 = 290.0;
const PLOT_X1: f32 = 965.0;
const PLOT_Y: f32 = 965.0;
const PLOT_H: f32 = 95.0;
const CIRCLE: vec2f = vec2f(178.0, 917.0);
const CIRCLE_R: f32 = 57.0;

// The ring of each pendulum, row by row from the top left: measured from the clip this
// rebuilds, so it is a table and no formula.
var<private> RINGS: array<u32, 100> = array<u32, 100>(
    9u, 8u, 7u, 6u, 6u, 6u, 6u, 7u, 8u, 9u,
    8u, 6u, 5u, 5u, 4u, 4u, 5u, 5u, 6u, 8u,
    7u, 5u, 4u, 3u, 3u, 3u, 3u, 4u, 5u, 7u,
    6u, 5u, 3u, 2u, 1u, 1u, 2u, 3u, 5u, 6u,
    6u, 4u, 3u, 1u, 0u, 0u, 1u, 3u, 4u, 6u,
    6u, 4u, 3u, 1u, 0u, 0u, 1u, 3u, 4u, 6u,
    6u, 5u, 3u, 2u, 1u, 1u, 2u, 3u, 5u, 6u,
    7u, 5u, 4u, 3u, 3u, 3u, 3u, 4u, 5u, 7u,
    8u, 6u, 5u, 5u, 4u, 4u, 5u, 5u, 6u, 8u,
    9u, 8u, 7u, 6u, 6u, 6u, 6u, 7u, 8u, 9u,
);
// Pendulums per ring.
var<private> COUNT: array<f32, 10> = array<f32, 10>(4.0, 8.0, 4.0, 16.0, 12.0, 16.0, 20.0, 8.0, 8.0, 4.0);

fn srgb(c: vec3f) -> vec3f {
    return pow(c, vec3f(2.2));
}

// Swings of ring k done at time t, wrapped to 0..1 (0: at the bottom, moving right).
fn swings(k: u32, t: f32, times: f32) -> f32 {
    return fract(t / T * f32(80u - k) * times);
}

// R(t) = |1/100 sum_j e^(2 i phi_j)|: 1 when every pendulum is at the bottom at once.
fn sync(t: f32) -> f32 {
    var s = vec2f(0.0);
    for (var k = 0u; k < 10u; k++) {
        let a = TAU * swings(k, t, 2.0);
        s += COUNT[k] * vec2f(cos(a), sin(a));
    }
    return length(s) / 100.0;
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
    let scale = pn.resolution.y / REF.y;
    let q = (v.pos.xy - vec2f((pn.resolution.x - REF.x * scale) * 0.5, 0.0)) / scale;
    let px = 1.0 / scale;
    let t = pn_time.seconds - T * floor(pn_time.seconds / T);

    var dark = 1.0;
    if params.look == look_light {
        dark = 0.0;
    } else if params.look == look_fade {
        dark = smoothstep(8.0, 12.0, t) * (1.0 - smoothstep(60.0, 64.0, t));
    }
    // The ground fades in sRGB; the drawing flips from dark to light ink halfway, where the
    // ground is mid grey, so it stays readable all through the fade.
    var col = srgb(mix(vec3f(1.0), vec3f(0.075, 0.08, 0.125), dark));
    let ink = smoothstep(0.45, 0.65, dark);
    let rod_col = mix(srgb(vec3f(0.38)), srgb(vec3f(0.68, 0.7, 0.76)), ink);
    let pivot_col = mix(srgb(vec3f(0.2)), srgb(vec3f(0.95)), ink);
    let bob_col = mix(srgb(vec3f(0.06, 0.06, 0.08)), srgb(vec3f(0.74, 0.76, 0.83)), ink);
    let axis_col = mix(srgb(vec3f(0.5)), srgb(vec3f(0.5, 0.52, 0.6)), ink);
    let text_col = mix(srgb(vec3f(0.15)), srgb(vec3f(0.92, 0.93, 0.96)), ink);
    let hot = srgb(vec3f(1.0, 0.97, 0.86));
    let curve_col = srgb(vec3f(0.98, 0.74, 0.22));

    // The pendulums: the nearest column and the rows whose bobs reach this pixel.
    let cell = (q - ORIGIN) / PITCH;
    let c0 = i32(round(cell.x));
    let r0 = i32(floor(cell.y));
    for (var dr = -1; dr <= 1; dr++) {
        for (var dc = -1; dc <= 1; dc++) {
            let c = c0 + dc;
            let r = r0 + dr;
            if c < 0 || c > 9 || r < 0 || r > 9 {
                continue;
            }
            let k = RINGS[r * 10 + c];
            let w = swings(k, t, 1.0);
            let angle = radians(params.swing) * sin(TAU * w);
            let pivot = ORIGIN + vec2f(f32(c), f32(r)) * PITCH;
            let bob = pivot + ROD * vec2f(sin(angle), cos(angle));
            // Seconds since this ring last passed the bottom: it does twice per swing.
            let since = fract(2.0 * w) * T / (2.0 * f32(80u - k));
            let fading = clamp(1.0 - since / max(params.glow, 1e-3), 0.0, 1.0);
            let g = fading * fading;
            col = mix(col, rod_col, cover(segment(q, pivot, bob), 1.1, px));
            col = mix(col, pivot_col, cover(length(q - pivot), 3.0, px));
            let d = length(q - bob);
            let halo = g * smoothstep(BOB + 16.0 * g, BOB, d);
            col = mix(col, params.glow_color.rgb, halo * params.glow_color.a);
            col = mix(col, mix(bob_col, hot, g), cover(d, BOB, px));
        }
    }

    // The sync plot: axes, ticks at T/5, T/3, T/2, 2T/3, 4T/5, R(t) up to now and a dot at now.
    var axis = cover(segment(q, vec2f(280.0, PLOT_Y), vec2f(1000.0, PLOT_Y)), 0.9, px);
    axis = max(axis, cover(segment(q, vec2f(1000.0, PLOT_Y), vec2f(990.0, PLOT_Y - 5.0)), 0.9, px));
    axis = max(axis, cover(segment(q, vec2f(1000.0, PLOT_Y), vec2f(990.0, PLOT_Y + 5.0)), 0.9, px));
    axis = max(axis, cover(segment(q, vec2f(PLOT_X0, PLOT_Y + 10.0), vec2f(PLOT_X0, 855.0)), 0.9, px));
    let ticks = array<f32, 5>(0.2, 1.0 / 3.0, 0.5, 2.0 / 3.0, 0.8);
    for (var i = 0; i < 5; i++) {
        let x = mix(PLOT_X0, PLOT_X1, ticks[i]);
        axis = max(axis, cover(segment(q, vec2f(x, PLOT_Y - 5.0), vec2f(x, PLOT_Y + 5.0)), 0.9, px));
    }
    // The phase circle: a dot per ring at the angle of e^(2 i phi), a mark at angle 0.
    let around = abs(length(q - CIRCLE) - CIRCLE_R);
    axis = max(axis, cover(around, 0.9, px));
    let mark = CIRCLE + vec2f(CIRCLE_R, 0.0);
    axis = max(axis, cover(segment(q, mark - vec2f(6.0, 0.0), mark + vec2f(6.0, 0.0)), 0.9, px));
    col = mix(col, axis_col, axis);

    let now_x = mix(PLOT_X0, PLOT_X1, t / T);
    if q.y > 850.0 && q.y < 975.0 && q.x >= PLOT_X0 - 2.0 && q.x <= now_x {
        let step = T / (PLOT_X1 - PLOT_X0);
        let tt = (q.x - PLOT_X0) * step;
        let y = PLOT_Y - PLOT_H * sync(tt);
        let slope = PLOT_H * (sync(tt + step) - sync(max(tt - step, 0.0))) * 0.5;
        let d = abs(q.y - y) / sqrt(1.0 + slope * slope);
        col = mix(col, curve_col, cover(d, 1.5, px));
    }
    let now = vec2f(now_x, PLOT_Y - PLOT_H * sync(t));
    col = mix(col, curve_col, cover(length(q - now), 7.0, px));
    if length(q - CIRCLE) < CIRCLE_R + 10.0 {
        for (var k = 0u; k < 10u; k++) {
            let a = TAU * swings(k, t, 2.0);
            let p = CIRCLE + CIRCLE_R * vec2f(cos(a), -sin(a));
            col = mix(col, curve_col, cover(length(q - p), 5.0, px));
        }
    }

    let labels = textureSampleLevel(in0, pn_clamp, q / REF, 0.0).a;
    col = mix(col, text_col, labels);
    return vec4f(col, 1.0);
}
