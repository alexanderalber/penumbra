// One mip of a texture the render samples (render/mips.rs): the level above, halved. A bilinear
// sample in the middle of each new texel averages the 2x2 texels under it; where a side is odd
// the last texel is left a little short. Premultiplied, so colour and alpha average alike.

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

struct V {
    @builtin(position) pos: vec4f,
    @location(0) uv: vec2f,
};

@vertex
fn vs(@builtin(vertex_index) i: u32) -> V {
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    var v: V;
    v.pos = vec4f(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    v.uv = vec2f(x, y);
    return v;
}

@fragment
fn fs(v: V) -> @location(0) vec4f {
    return textureSampleLevel(src, samp, v.uv, 0.0);
}
