// The carving of floor_gates' gate, as a still picture: it cooks once and again only when
// `seed` changes. Render it 2048 x 1024. Grey levels only: the light's colour and its rise with
// the swell and the beat come from the placement shader (gates.wgsl) as each copy's colour.
// Cells (gates.py writes the uvs): v 0 to 0.16 the lintel's front, a running step fret between
// two grooves; u 0 to 0.125 and 0.125 to 0.25 at v 0.18 to 0.96 the pillars' fronts, a column of
// signs each. v 0.97 to 1 is the light ramp of the body: there the grey is u.
// @param seed int = 0 0..1000 -- other erosion and other signs
// @param groove float = 1 0..2 -- how bright a glowing groove's floor is against the rest

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

// The floor of a groove glows, eroded and with stretches worn away; the stone stays dark.
fn carve(d: f32, p: vec2f, w0: f32, k: u32) -> f32 {
    let w = w0 * (1.0 + 0.35 * fbm(p * 5.0, 9u + k));
    let e = d + fbm(p * 26.0, 3u + k) * w0 * 0.3 - w;
    let worn = smoothstep(0.1, 0.35, fbm(p * 1.7, 11u + k));
    let inside = (1.0 - smoothstep(-0.003, 0.003, e)) * (1.0 - worn);
    let deep = clamp(-e / w, 0.0, 1.0);
    let smoulder = 0.5 + 0.5 * fbm(p * 6.0, 21u + k);
    return inside * pow(deep, 2.0) * smoulder * params.groove;
}

// The lintel: p.x 0..14 along it, p.y 0..1 down.
fn lintel(p: vec2f) -> f32 {
    let cell = floor(p.x);
    let q = vec2f(fract(p.x) - 0.5, p.y - 0.5) / 0.34;
    let k = select(q, vec2f(-q.x, q.y), (i32(cell) & 1) == 1);
    var d = fret(k) * 0.34;
    d = min(d, min(abs(p.y - 0.07), abs(p.y - 0.93)));
    return carve(d, p, 0.022, 0u);
}

// A pillar: p.x 0..1 across, p.y 0..6.7 down; `side` 0 the left one, 1 the right one.
fn pillar(p: vec2f, side: u32) -> f32 {
    var d = 1e9;
    let s = u32(max(params.seed, 0));
    for (var i = 0u; i < 5u; i++) {
        let c = vec2f(0.5, 0.75 + f32(i) * 1.25);
        let q = (p - c) / 0.3;
        let pick = (i + side * 2u + s) % 4u;
        var g = 1e9;
        if pick == 0u {
            g = spiral(q, 6u);
        } else if pick == 1u {
            g = fret(q);
        } else if pick == 2u {
            g = min(abs(length(q) - 0.7), length(q) - 0.15);
        } else {
            g = fret(vec2f(-q.x, q.y));
        }
        d = min(d, g * 0.3);
    }
    return carve(d, p, 0.018, 4u + side);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let uv = v.uv;
    if uv.y > 0.97 {
        // Dark stays dark: only the brightest rims come through strongly.
        return vec4f(vec3f(1.6 * pow(uv.x, 2.2)), 1.0);
    }
    let grain = 0.002 + 0.002 * fbm(uv * vec2f(300.0, 150.0), 5u);
    var g = 0.0;
    if uv.y < 0.16 {
        g = lintel(vec2f(uv.x * 14.0, uv.y / 0.16));
    } else if uv.y > 0.18 && uv.y < 0.96 && uv.x < 0.25 {
        let side = select(0u, 1u, uv.x >= 0.125);
        let lx = fract(uv.x / 0.125);
        g = pillar(vec2f(lx, (uv.y - 0.18) / 0.78 * 6.7), side);
    }
    return vec4f(vec3f(grain + g), 1.0);
}
