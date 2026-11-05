// The light that falls into floor_cenote's ring, as a still picture: it cooks once. Narrow high up,
// wider at the water, made of soft streaks, and gone before the picture's top. It is light with
// almost no alpha: it adds to what lies behind it and stays out of the render's data pass. Render
// it 256 x 512. Its brightness comes from the placement shader.

fn hash1(i: i32) -> f32 {
    return f32(pn_hash(vec3u(bitcast<u32>(i), 53u, 0u)).x) / 4294967296.0;
}

fn noise1(x: f32) -> f32 {
    let i = i32(floor(x));
    let f = fract(x);
    return mix(hash1(i), hash1(i + 1), f * f * (3.0 - 2.0 * f));
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let uv = v.pos.xy / pn.resolution;
    let h = 1.0 - uv.y;
    let x = 2.0 * uv.x - 1.0;
    let width = mix(0.8, 0.25, h);
    let across = x / width;
    let body = exp(-across * across * 2.5);
    let streaks = 0.45 + 0.55 * noise1(across * 7.0 + 3.0) * (0.6 + 0.4 * noise1(across * 19.0));
    let along = smoothstep(0.0, 0.03, h) * (1.0 - smoothstep(0.1, 1.0, h));
    let edge = 1.0 - smoothstep(0.8, 1.0, abs(x));
    let light = vec3f(0.42, 0.85, 0.62) * body * streaks * along * edge;
    return vec4f(light, select(0.0, 0.003, light.g > 0.0005));
}
