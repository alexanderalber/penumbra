// The carving of floor_stelae's stones, as a still picture: it cooks once and again only when
// `seed` changes. Render it 2048 x 2048. Grey levels only: the light's colour and its rise with
// the swell and the beat come from the placement shader (stelae.wgsl) as each copy's colour.
// Cells (stelae.py writes the uvs): u 0 to 0.25 and 0.25 to 0.5 the two glyph panels of the
// monolith (a and g), u 0.5 to 0.625 the relief of the slender stone (c), u 0.625 to 1 and v 0 to
// 0.375 the disc (e). v 0.97 to 1 is the light ramp of the bodies: there the grey is u.
// @param seed int = 0 0..1000 -- other erosion and other signs on the disc
// @param groove float = 1 0..2 -- how bright a glowing groove's floor is against the rest

const CELL_V: f32 = 0.96;
const PI: f32 = 3.14159265;

fn seg(p: vec2f, a: vec2f, b: vec2f) -> f32 {
    let pa = p - a;
    let ba = b - a;
    let h = clamp(dot(pa, ba) / max(dot(ba, ba), 1e-8), 0.0, 1.0);
    return length(pa - ba * h);
}

fn hash2(i: vec2i, k: u32) -> f32 {
    return f32(pn_hash(vec3u(bitcast<u32>(i.x), bitcast<u32>(i.y), k + u32(max(params.seed, 0)) * 31u)).x) / 4294967296.0;
}

fn vnoise(p: vec2f, k: u32) -> f32 {
    let i = vec2i(floor(p));
    let f = fract(p);
    let s = f * f * (3.0 - 2.0 * f);
    let a = hash2(i, k);
    let b = hash2(i + vec2i(1, 0), k);
    let c = hash2(i + vec2i(0, 1), k);
    let d = hash2(i + vec2i(1, 1), k);
    return mix(mix(a, b, s.x), mix(c, d, s.x), s.y) * 2.0 - 1.0;
}

fn fbm(p: vec2f, k: u32) -> f32 {
    var a = 0.5;
    var q = p;
    var t = 0.0;
    for (var i = 0u; i < 5u; i++) {
        t += a * vnoise(q, k + i * 7u);
        q = q * 2.03 + vec2f(1.7, 9.2);
        a *= 0.5;
    }
    return t;
}

// A squared spiral from its middle out, `turns` legs.
fn spiral(q: vec2f, turns: u32) -> f32 {
    let dirs = array<vec2f, 4>(vec2f(1.0, 0.0), vec2f(0.0, 1.0), vec2f(-1.0, 0.0), vec2f(0.0, -1.0));
    let unit = 0.4;
    var at = vec2f(0.0);
    var d = 1e9;
    for (var i = 0u; i < turns; i++) {
        let len = unit * f32(i / 2u + 1u);
        let next = at + dirs[i % 4u] * len;
        d = min(d, seg(q, at, next));
        at = next;
    }
    return d;
}

// A step fret: a staircase that ends in a hook.
fn fret(q: vec2f) -> f32 {
    let pts = array<vec2f, 9>(
        vec2f(-0.9, 0.9), vec2f(-0.3, 0.9), vec2f(-0.3, 0.3), vec2f(0.3, 0.3), vec2f(0.3, -0.9),
        vec2f(0.9, -0.9), vec2f(0.9, -0.1), vec2f(0.6, -0.1), vec2f(0.6, -0.55),
    );
    var d = 1e9;
    for (var i = 0; i < 8; i++) {
        d = min(d, seg(q, pts[i], pts[i + 1]));
    }
    return d;
}

fn box(p: vec2f, c: vec2f, h: vec2f) -> f32 {
    let e = abs(p - c) - h;
    return length(max(e, vec2f(0.0))) + min(max(e.x, e.y), 0.0);
}

fn rbox(p: vec2f, c: vec2f, h: vec2f, r: f32) -> f32 {
    return box(p, c, h - vec2f(r)) - r;
}

// A number 1 to 13 in dots over bars, centred at x = cx from y = top, unit u.
fn number(p: vec2f, cx: f32, top: f32, value: u32, u: f32) -> f32 {
    var d = 1e9;
    let n = value % 5u;
    for (var i = 0u; i < n; i++) {
        let x = cx + (f32(i) - 0.5 * f32(n - 1u)) * 0.16 * u;
        d = min(d, length(p - vec2f(x, top)) - 0.035 * u);
    }
    for (var i = 0u; i < value / 5u; i++) {
        let y = top + (0.16 + f32(i) * 0.15) * u;
        d = min(d, box(p, vec2f(cx, y), vec2f(0.28, 0.035) * u));
    }
    return d;
}

// --- the monolith's glyph panels (a and g) ---------------------------------------------------

const ASP_A: f32 = 2.7;

// Distance to the cut lines of panel c at p (x 0..1 across the front, y 0..ASP_A down it).
fn glyph_a(p: vec2f, c: u32) -> f32 {
    var d = abs(box(p, vec2f(0.5, ASP_A * 0.5 + 0.05), vec2f(0.4, ASP_A * 0.5 - 0.12)));
    let a = (p - vec2f(0.5, 0.62)) / 0.27;
    let b = (p - vec2f(0.5, 1.32)) / 0.27;
    var g1 = 1e9;
    var g2 = 1e9;
    if c == 0u {
        g1 = spiral(a, 8u);
        g2 = fret(b);
    } else {
        g1 = fret(vec2f(-a.x, a.y));
        g2 = min(abs(length(b) - 0.75), length(b) - 0.18);
    }
    d = min(d, min(g1, g2) * 0.27);
    d = min(d, number(p, 0.5, 1.95, 3u + c * 4u, 1.1));
    return d;
}

// The floors of the eroded grooves glow; their walls and the stone stay dark. Stretches of a
// groove are worn away.
fn panel_a(uv: vec2f, c: u32) -> f32 {
    let p = vec2f(uv.x, uv.y / CELL_V * ASP_A);
    let w = 0.016 + 0.006 * fbm(p * 6.0, 9u + c);
    let d = glyph_a(p, c) + fbm(p * 30.0, 3u + c) * 0.005 - w;
    let worn = smoothstep(0.05, 0.3, fbm(p * 2.6 + vec2f(f32(c) * 5.0, 0.0), 11u + c));
    let inside = (1.0 - smoothstep(-0.003, 0.003, d)) * (1.0 - worn);
    let deep = clamp(-d / w, 0.0, 1.0);
    let smoulder = 0.5 + 0.5 * fbm(p * 7.0, 21u);
    let grain = 0.002 + 0.003 * fbm(p * 40.0, 5u);
    return grain + inside * pow(deep, 2.5) * smoulder * params.groove;
}

// --- the slender stone's relief (c) -------------------------------------------------------------

const ASP_C: f32 = 6.4;

// Height of the relief at p (x 0..1 across, y 0..ASP_C down): five raised panels of different
// heights with rounded corners, a sign cut into each, all edges soft. Only the panel nearest p
// counts: the panels are far enough apart for their soft edges not to meet.
fn relief_c(p: vec2f) -> f32 {
    let tops = array<f32, 6>(0.25, 1.35, 2.65, 3.55, 4.85, 6.15);
    var i = 0u;
    for (var k = 1u; k < 5u; k++) {
        if p.y > tops[k] {
            i = k;
        }
    }
    let y0 = tops[i] + 0.09;
    let y1 = tops[i + 1u] - 0.09;
    let c = vec2f(0.5, 0.5 * (y0 + y1));
    let half = vec2f(0.36, 0.5 * (y1 - y0));
    let worn = fbm(p * 9.0, 41u) * 0.03;
    let panel = 1.0 - smoothstep(-0.03, 0.03, rbox(p, c, half, 0.07) + worn);
    let q = (p - c) / 0.26;
    var g = 1e9;
    let pick = (i + u32(max(params.seed, 0))) % 5u;
    if pick == 0u {
        g = spiral(q, 6u) * 0.26;
    } else if pick == 1u {
        g = fret(q * 1.05) / 1.05 * 0.26;
    } else if pick == 2u {
        g = abs((min(length(q - vec2f(-0.45, -0.5)), min(length(q - vec2f(0.45, -0.5)), length(q - vec2f(0.0, 0.35)))) - 0.18) * 0.26 + 0.03);
    } else if pick == 3u {
        g = min(abs(length(q) - 0.62), length(q) - 0.12) * 0.26;
    } else {
        g = abs(number(p, c.x, c.y - 0.12, 7u + i, 1.0)) * 0.6;
    }
    let cut = 1.0 - smoothstep(0.012, 0.03, g);
    // A panel stands a little proud of the stone; its sign is cut deeper than the stone.
    return 0.6 + panel * (0.4 - 1.0 * cut);
}

// Light grazing in from the front's band-side edge (+x) and a little from above: the walls of the
// relief that face it catch it, more the nearer they are to that edge. No outlines: a wall turned
// away stays dark.
fn front_c(uv: vec2f) -> f32 {
    let p = vec2f(uv.x, uv.y / CELL_V * ASP_C);
    let e = 0.006;
    let h = relief_c(p);
    let hx = (relief_c(p + vec2f(e, 0.0)) - h) / e;
    let hy = (relief_c(p + vec2f(0.0, e)) - h) / e;
    let facing = max(0.0, -hx * 0.95 + hy * 0.25) * 0.025;
    let graze = mix(0.08, 1.0, pow(smoothstep(0.05, 0.98, p.x), 1.5));
    let edge = pow(smoothstep(0.9, 1.0, p.x), 3.0) * 0.4;
    let grain = 0.002 + 0.002 * fbm(p * 30.0, 13u);
    return grain + min(facing, 1.2) * graze + edge;
}

// --- the disc (e) -----------------------------------------------------------------------------

// Distance to the disc's cut lines at p (-1..1 across the disc): circles, a squared spiral in
// the middle, step frets and dots in the inner ring, a running fret, then a ring of thirteen
// cartouches with numbers in dots and bars.
fn cuts_e(p: vec2f) -> f32 {
    let r = length(p);
    let a = atan2(p.y, p.x);
    var d = 1e9;
    d = min(d, abs(r - 0.21));
    d = min(d, abs(r - 0.5));
    d = min(d, abs(r - 0.74));
    d = min(d, abs(r - 0.95));
    d = min(d, spiral(p / 0.07, 7u) * 0.07);
    // Inner ring: four step frets on the diagonals, four dots on the axes.
    let quarter = round(a / (0.5 * PI));
    let qa = a - quarter * 0.5 * PI;
    let local = vec2f(r * cos(qa), r * sin(qa));
    d = min(d, abs(length(local - vec2f(0.355, 0.0)) - 0.035));
    let diag = a - (floor(a / (0.5 * PI)) + 0.5) * 0.5 * PI;
    let dl = vec2f(r * cos(diag) - 0.355, r * sin(diag));
    d = min(d, fret(vec2f(dl.y, -dl.x) / 0.085) * 0.085);
    // Middle ring: a running step fret of 16 cells, every other one turned round.
    let n = 16.0;
    let s = (a / (2.0 * PI) + 0.5) * n;
    let cell = floor(s);
    let t = vec2f((fract(s) - 0.5) * 2.0, (r - 0.62) / 0.08);
    let k = select(t, vec2f(-t.x, t.y), (i32(cell) & 1) == 1);
    let arc = 2.0 * PI * 0.62 / n * 0.5;
    d = min(d, fret(k * vec2f(1.0, 1.0)) * min(arc, 0.08));
    // Outer ring: thirteen cartouches, each with a number.
    let m = 13.0;
    let ci = round(a / (2.0 * PI) * m);
    let ca = ci / m * 2.0 * PI;
    let q = vec2f(r * cos(a - ca) - 0.845, r * sin(a - ca));
    d = min(d, abs(rbox(q, vec2f(0.0), vec2f(0.075, 0.1), 0.03)));
    let value = u32(i32(ci) + 13 + i32(max(params.seed, 0))) % 13u + 1u;
    d = min(d, number(vec2f(q.y, q.x), 0.0, -0.035, value, 0.28));
    return d;
}

// The grooves' floors take the glow; their upper walls catch a little light from above, the rest
// of the stone stays dark. The swell fills the rings with light through the four ring copies.
fn disc_e(uv: vec2f) -> f32 {
    let p = (vec2f((uv.x - 0.625) / 0.375, uv.y / 0.375) - 0.5) * 2.0;
    let r = length(p);
    if r > 1.0 {
        return 0.0;
    }
    let w = 0.014;
    let d0 = cuts_e(p);
    let d = d0 + fbm(p * 40.0, 31u) * 0.003;
    let floor_glow = pow(1.0 - smoothstep(0.0, w, d), 2.0) * (0.55 + 0.45 * fbm(p * 9.0, 33u));
    let e = 0.004;
    let up = cuts_e(p + vec2f(0.0, -e)) - d0;
    // A cut's lower wall faces up: there the distance falls towards the cut going down.
    let wall = (1.0 - smoothstep(w, 1.8 * w, d)) * smoothstep(0.0, 0.5, up / e) * 0.15;
    let bevel = (1.0 - smoothstep(0.0, 0.03, abs(r - 0.985))) * smoothstep(-0.2, -0.9, p.y) * 0.35;
    let grain = 0.002 + 0.002 * fbm(p * 30.0, 37u);
    return grain + floor_glow * params.groove + wall + bevel;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let uv = v.uv;
    if uv.y > 0.97 {
        // Dark stays dark: only the brightest rims come through strongly.
        return vec4f(vec3f(1.6 * pow(uv.x, 2.2)), 1.0);
    }
    if uv.y > CELL_V {
        return vec4f(0.0, 0.0, 0.0, 1.0);
    }
    var g = 0.0;
    if uv.x < 0.25 {
        g = panel_a(vec2f(uv.x / 0.25, uv.y), 0u);
    } else if uv.x < 0.5 {
        g = panel_a(vec2f((uv.x - 0.25) / 0.25, uv.y), 1u);
    } else if uv.x < 0.625 {
        g = front_c(vec2f((uv.x - 0.5) / 0.125, uv.y));
    } else if uv.y < 0.375 {
        g = disc_e(uv);
    }
    return vec4f(vec3f(g), 1.0);
}
