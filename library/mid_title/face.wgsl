// The face of the lettering: dark stone with ember light coming through it, drawn across the whole
// block of text by the front's texture coordinates. Cooked once, for a picture that does not move;
// the light on it comes from the copy's colour. Brightest low in the letters, as if lit from below.
// @param seed int = 0 0..1000 -- which veins
// @param warmth float = 0 0..1 -- 0 ember, 1 jade

fn hash2(p: vec2f) -> f32 {
    return fract(sin(dot(p, vec2f(127.1, 311.7))) * 43758.5453);
}

fn noise2(p: vec2f) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let low = mix(hash2(i), hash2(i + vec2f(1.0, 0.0)), u.x);
    let high = mix(hash2(i + vec2f(0.0, 1.0)), hash2(i + vec2f(1.0, 1.0)), u.x);
    return mix(low, high, u.y);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = vec2f(v.uv.x * 24.0, v.uv.y * 2.5) + f32(params.seed) * 7.3;
    let veins = noise2(p) * 0.6 + noise2(p * 2.3) * 0.3 + noise2(p * 5.1) * 0.1;
    let low = v.uv.y * v.uv.y;
    let heat = (0.07 + 0.55 * low * sqrt(v.uv.y)) * (0.5 + 1.0 * smoothstep(0.4, 0.7, veins));
    let hot = mix(vec3f(1.0, 0.36, 0.1), vec3f(0.42, 1.0, 0.72), clamp(params.warmth, 0.0, 1.0));
    return vec4f(hot * heat, 1.0);
}
