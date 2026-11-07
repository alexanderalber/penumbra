// tex.noise: fractal noise from integer hashes, so every backend renders the same picture.

fn layer(p: vec3f, seed: u32) -> f32 {
    var sum = 0.0;
    var amp = 1.0;
    var norm = 0.0;
    var q = p;
    for (var o = 0; o < params.octaves; o++) {
        let s = seed + u32(o) * 7919u;
        var n: f32;
        if (params.basis == basis_value) {
            n = pn_value_noise(q, s);
        } else {
            n = pn_noise(q, s) * 1.6;
        }
        sum += n * amp;
        norm += amp;
        amp *= params.roughness;
        q = q * params.lacunarity + vec3f(17.3, -9.1, 5.7);
    }
    return sum / max(norm, 1e-6);
}

// One independent draw per pixel: the hash takes the pixel's integer position, and the frame
// when the picture renews, so neighbours share nothing and nothing is interpolated.
fn random_pixel(pos: vec2f, seed: u32) -> vec3f {
    var frame = 0;
    if (params.every_frame != 0u) {
        frame = i32(pn_time.frame);
    }
    let h = pn_cell(vec3i(vec2i(pos), frame), seed);
    if (params.color != 0u) {
        return h;
    }
    return vec3f(h.x);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let seed = bitcast<u32>(params.seed) * 3u;
    var c: vec3f;
    if (params.basis == basis_random) {
        c = random_pixel(v.pos.xy, seed);
    } else {
        let aspect = pn.resolution.x / pn.resolution.y;
        let xy = (v.uv - 0.5) * vec2f(aspect, 1.0) * params.frequency;
        let p = vec3f(xy, pn_time.seconds * params.speed) + params.translate;
        let r = layer(p, seed);
        c = vec3f(r);
        if (params.color != 0u) {
            c = vec3f(r, layer(p, seed + 1u), layer(p, seed + 2u));
        }
    }
    let a = clamp(params.alpha, 0.0, 1.0);
    return vec4f((params.offset + params.amplitude * c) * a, a);
}
