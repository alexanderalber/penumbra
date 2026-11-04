// The faces of the stelae: four cells side by side, each a stela's front, as light on a black
// stone. Cell 0 is black (the stela's other faces), cell 1 a squared spiral, cell 2 a step fret
// (a staircase ending in a hook), cell 3 a square of dots. Under each glyph a number in dots
// and bars, framed by a carved border. The glyphs are carved: their grooves stay dark, and the
// lower wall and one side of each groove catch the light. Render it 512 x 384.
// @param seed int = 0 0..1000 -- other spirals, frets, dots and numbers
// @param groove float = 0.12 0..1 -- how bright a groove's floor is, against 1 for its lit wall
// @param stroke float = 0.045 0.01..0.12 -- a groove's half width, in face widths

fn seg(p: vec2f, a: vec2f, b: vec2f) -> f32 {
    let pa = p - a;
    let ba = b - a;
    let h = clamp(dot(pa, ba) / max(dot(ba, ba), 1e-8), 0.0, 1.0);
    return length(pa - ba * h);
}

fn rnd(i: u32, k: u32) -> f32 {
    return f32(pn_hash(vec3u(i, k, u32(max(params.seed, 0)) + 7u)).x) / 4294967296.0;
}

// A squared spiral from the middle out, in a glyph's square (-1..1, y down), as a distance.
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

// A step fret: a staircase rising to the right that ends in a square hook.
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

// Dots in a 3 x 3 square: the four corners and the middle, and some of the others by `pick`.
fn dots(q: vec2f, pick: u32) -> f32 {
    var d = 1e9;
    let shown = pick | 0x155u;
    for (var i = 0u; i < 9u; i++) {
        if ((shown >> i) & 1u) == 0u {
            continue;
        }
        let c = vec2f(f32(i % 3u) - 1.0, f32(i / 3u) - 1.0) * 0.62;
        d = min(d, max(length(q - c) - 0.05, 0.0));
    }
    return d;
}

// A number from 1 to 13 in dots (ones) over bars (fives), centred at x = 0.5 from y = top.
fn number(p: vec2f, top: f32, value: u32) -> f32 {
    var d = 1e9;
    let n = value % 5u;
    for (var i = 0u; i < n; i++) {
        let x = 0.5 + (f32(i) - 0.5 * f32(n - 1u)) * 0.16;
        d = min(d, max(length(p - vec2f(x, top)) - 0.035, 0.0));
    }
    for (var i = 0u; i < value / 5u; i++) {
        let y = top + 0.16 + f32(i) * 0.15;
        let e = abs(p - vec2f(0.5, y)) - vec2f(0.28, 0.035);
        d = min(d, length(max(e, vec2f(0.0))));
    }
    return d;
}

// The grooves of cell `cell` at p (x 0..1 across the face, y 0..3 down it): 1 inside, 0 outside.
fn grooves(p: vec2f, cell: u32) -> f32 {
    let w = params.stroke;
    // The border, inset from the face's edges; the lowest part of the stone stays plain.
    let e = abs(p - vec2f(0.5, 1.3)) - vec2f(0.38, 1.12);
    var d = abs(max(e.x, e.y));
    let q = (p - vec2f(0.5, 0.72)) / 0.33;
    let mirror = rnd(cell, 1u) < 0.5;
    let qm = select(q, vec2f(-q.x, q.y), mirror);
    var g = 1e9;
    if cell == 1u {
        g = spiral(qm, 7u + u32(rnd(cell, 2u) * 2.0));
    } else if cell == 2u {
        g = fret(qm);
    } else {
        g = dots(q, pn_hash(vec3u(cell, 3u, u32(max(params.seed, 0)))).x);
    }
    d = min(d, g * 0.33);
    d = min(d, number(p, 1.35, 1u + u32(rnd(cell, 4u) * 13.0)));
    return 1.0 - smoothstep(w - 0.006, w + 0.006, d);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let cells = 4.0;
    let x = v.uv.x * cells;
    let cell = u32(clamp(floor(x), 0.0, 3.0));
    if cell == 0u {
        return vec4f(0.0, 0.0, 0.0, 1.0);
    }
    let p = vec2f(fract(x), v.uv.y * 3.0);
    let m = grooves(p, cell);
    // The groove's lower wall and one side: inside the groove, with stone just below and beside.
    let below = grooves(p + vec2f(0.018, 0.025), cell);
    let lit = clamp(m - below, 0.0, 1.0);
    let c = params.groove * m + (1.0 - params.groove) * lit;
    return vec4f(vec3f(c), 1.0);
}
