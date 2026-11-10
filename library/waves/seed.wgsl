// The start of the ripples, in the state layout of step.wgsl: r = height now, g = height one
// step before, b = energy mean, a = people brightness. Its size is the simulation's (in0).
// Dents are shaped as in step.wgsl (minus a Laplacian), so they add up to 0: the water level
// starts at 0.
//
// in0: the simulation's size (the component's world). in1: the people input, any size.
//
// @param start choice(calm|drop|three|line) = calm -- the water at the start: calm: still water; drop: one drop in the middle; three: three drops; line: a straight dent across, at the left third
// @param start_size float = 12 2..80 -- radius of the dents at the start (half width of the line), in texels
// @param start_rings int = 3 1..8 -- dents per drop at the start: 1 is one smooth dent, more a train of rings
// @param start_strength float = 1.0 -2..2 -- depth of the dents at the start

var<private> n: vec2f;
var<private> size: f32;
var<private> rings: f32;

fn hump(d: f32, r: f32, k: f32) -> f32 {
    let t = clamp(d / (r * k), 0.0, 1.0);
    let win = 0.5 + 0.5 * cos(3.14159265 * t);
    return win * cos(6.2831853 * (k - 1.0) * t);
}

// The height before the Laplacian, at a texel centre q.
fn g(q: vec2f) -> f32 {
    if params.start == start_drop {
        return hump(distance(q, n * 0.5), size, rings);
    }
    if params.start == start_three {
        return hump(distance(q, n * vec2f(0.3, 0.35)), size, rings) + hump(distance(q, n * vec2f(0.62, 0.62)), size, rings)
            + hump(distance(q, n * vec2f(0.75, 0.3)), size, rings);
    }
    if params.start == start_line {
        let t = clamp(abs(q.x - n.x * 0.33) / size, 0.0, 1.0);
        return 0.5 + 0.5 * cos(3.14159265 * t);
    }
    return 0.0;
}

fn at(q: vec2i) -> f32 {
    let c = clamp(q, vec2i(0), vec2i(n) - vec2i(1));
    return g(vec2f(c) + 0.5);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    n = pn.resolution;
    size = max(params.start_size, 1.0);
    rings = max(f32(params.start_rings), 1.0);
    let p = vec2i(v.pos.xy);
    let w = select(3.14159265 / size, 6.2831853 * (rings - 1.0) / (size * rings), rings > 1.5 && params.start != start_line);
    let scale = 2.0 * (1.0 - cos(w));
    let lap = at(p + vec2i(1, 0)) + at(p - vec2i(1, 0)) + at(p + vec2i(0, 1)) + at(p - vec2i(0, 1)) - 4.0 * at(p);
    let h = lap / max(scale, 1e-4) * params.start_strength;
    let m = clamp(pn_luma(textureSampleLevel(in1, pn_clamp, v.uv, 0.0).rgb), 0.0, 1.0);
    return vec4f(h, h, 0.0, m);
}
