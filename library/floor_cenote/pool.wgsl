// The water in floor_cenote's ring, as a still picture: it cooks once. Render it 1024 x 1024. Deep
// jade in the middle, paler towards the wall, with a faint net of caustics. The hot spots lie on
// top of it (hot.wgsl), and its brightness comes from the placement shader.

fn hash2(i: vec2i) -> vec2f {
    let h = pn_hash(vec3u(bitcast<u32>(i.x), bitcast<u32>(i.y), 31u));
    return vec2f(f32(h.x), f32(h.y)) / 4294967296.0;
}

// The distance to the nearest cell edge of a Voronoi net.
fn net(p: vec2f) -> f32 {
    let g = vec2i(floor(p));
    let f = fract(p);
    var first = 9.0;
    var second = 9.0;
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let c = vec2f(f32(x), f32(y)) + hash2(g + vec2i(x, y)) - f;
            let d = dot(c, c);
            if d < first {
                second = first;
                first = d;
            } else if d < second {
                second = d;
            }
        }
    }
    return sqrt(second) - sqrt(first);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let at = 2.0 * v.pos.xy / pn.resolution - 1.0;
    let r = min(length(at), 1.0);
    let deep = vec3f(0.002, 0.024, 0.018);
    let pale = vec3f(0.005, 0.065, 0.048);
    let body = mix(deep, pale, r * r);
    // Bent, so the net reads as light on moving water and not as cells.
    let bend = at + 0.09 * vec2f(sin(at.y * 9.0 + 1.0), sin(at.x * 7.0 + 2.0));
    let lines = 1.0 - smoothstep(0.0, 0.05, net(bend * 11.0));
    return vec4f(body + vec3f(0.004, 0.03, 0.022) * lines, 1.0);
}
