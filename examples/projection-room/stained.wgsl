// Leaded glass for a window pane: coloured cells, see-through (alpha `glass`), between opaque
// dark lead lines and a lead frame. Premultiplied, as every texture.
// @param cells float = 4 1..20 -- cells across
// @param glass float = 0.7 0.05..1 -- the cells' alpha: more is more colour, less light
// @param lead float = 0.07 0..0.3 -- width of the lead lines, in cells
// @param seed float = 3 0..100
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let lead_colour = vec4f(0.02, 0.018, 0.015, 1.0);
    let edge = min(min(v.uv.x, 1.0 - v.uv.x), min(v.uv.y, 1.0 - v.uv.y));
    if edge < 0.025 {
        return lead_colour;
    }
    let p = v.uv * vec2f(params.cells, params.cells * pn.resolution.y / pn.resolution.x);
    let cell = floor(p);
    let seed = u32(params.seed);
    var d1 = 9.0;
    var d2 = 9.0;
    var id = vec2i(0);
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let c = vec2i(cell) + vec2i(x, y);
            let h = pn_hash(vec3u(vec2u(c + vec2i(1000)), seed));
            let jitter = vec2f(h.xy & vec2u(0xffffu)) / 65535.0;
            let d = length(p - (vec2f(c) + 0.15 + 0.7 * jitter));
            if d < d1 {
                d2 = d1;
                d1 = d;
                id = c;
            } else if d < d2 {
                d2 = d;
            }
        }
    }
    if d2 - d1 < params.lead {
        return lead_colour;
    }
    let h = pn_hash(vec3u(vec2u(id + vec2i(1000)), seed + 17u));
    let hue = f32(h.x & 0xffffu) / 65535.0;
    let rgb = clamp(abs(fract(vec3f(hue) + vec3f(0.0, 2.0 / 3.0, 1.0 / 3.0)) * 6.0 - 3.0) - 1.0, vec3f(0.0), vec3f(1.0));
    // Deep colours, to linear light.
    let colour = pow(mix(rgb, vec3f(1.0), 0.1), vec3f(2.2));
    return vec4f(colour * params.glass, params.glass);
}
