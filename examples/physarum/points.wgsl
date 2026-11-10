// A stand-in for a food picture: bright dots at random places, kept apart by a jittered grid.
// Later: a picture of real things (oat flakes on a table, a map of cities, tracked objects).
//
// @param count int = 12 1..64 -- number of dots
// @param dot_size float = 0.012 0.002..0.05 -- dot radius in picture heights
// @param seed int = 3 0..1000 -- another seed puts them elsewhere

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let aspect = pn.resolution.x / pn.resolution.y;
    let q = vec2f(v.uv.x * aspect, v.uv.y);
    // Cells of a grid with about as many cells as dots, one dot per cell somewhere inside.
    let cols = max(1, i32(round(sqrt(f32(params.count) * aspect))));
    let rows = max(1, (params.count + cols - 1) / cols);
    var c = 0.0;
    for (var i = 0; i < params.count; i++) {
        let h = pn_hash(vec3u(u32(i), u32(params.seed), 5u));
        let j = vec2f(vec2u(h.xy) >> vec2u(8u)) / 16777216.0;
        let cell = vec2f(f32(i % cols), f32(i / cols));
        let at = (cell + 0.15 + 0.7 * j) / vec2f(f32(cols), f32(rows));
        let d = length(q - vec2f(at.x * aspect, at.y));
        c = max(c, 1.0 - smoothstep(params.dot_size * 0.7, params.dot_size, d));
    }
    return vec4f(vec3f(c), 1.0);
}
