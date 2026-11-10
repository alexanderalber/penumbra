// A few bright dots: a stand-in for bright points in a camera picture (LEDs, pins, studs).
// Into the dots input of `growth`: with dot_seeds on, a crystal starts at each.
// @param count int = 7 0..64 -- how many dots
// @param size float = 4 1..40 -- dot radius in texels
// @param seed int = 3 0..1000 -- other places

fn rand(i: u32, k: u32) -> f32 {
    let h = pn_hash(vec3u(i, k, u32(params.seed) * 7919u + 5u));
    return f32(h.x >> 8u) / 16777216.0;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let n = pn.resolution;
    var d = 0.0;
    for (var i = 0; i < params.count; i++) {
        let c = vec2f(0.08 + 0.84 * rand(u32(i), 1u), 0.1 + 0.8 * rand(u32(i), 2u)) * n;
        d = max(d, 1.0 - smoothstep(params.size - 1.0, params.size, length(v.pos.xy - c)));
    }
    return vec4f(d, d, d, 1.0);
}
