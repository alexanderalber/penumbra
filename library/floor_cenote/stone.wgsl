// The stone of floor_cenote's ring, as a still picture: it cooks once. Render it 512 x 256. Grey
// levels only: the light's colour comes from the placement shader (cenote.wgsl) as each copy's
// colour. u is a face's light (cenote.py writes it), v its place round the ring, for some grain.

fn hash2(i: vec2i) -> f32 {
    return f32(pn_hash(vec3u(bitcast<u32>(i.x), bitcast<u32>(i.y), 7u)).x) / 4294967296.0;
}

fn vnoise(p: vec2f) -> f32 {
    let i = vec2i(floor(p));
    let f = fract(p);
    let s = f * f * (3.0 - 2.0 * f);
    return mix(mix(hash2(i), hash2(i + vec2i(1, 0)), s.x), mix(hash2(i + vec2i(0, 1)), hash2(i + vec2i(1, 1)), s.x), s.y);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let uv = v.pos.xy / pn.resolution;
    let grain = 0.75 + 0.5 * vnoise(vec2f(uv.x * 24.0, uv.y * 180.0));
    return vec4f(vec3f(uv.x * grain), 1.0);
}
