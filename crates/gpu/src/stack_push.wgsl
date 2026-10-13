// Frame stacks (stack.rs): one frame written into a layer, averaged down to the stack's size.

struct Push {
    // Source texels per target texel.
    footprint: vec2f,
    // One source texel in uv.
    src_texel: vec2f,
};

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> push: Push;

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

// Mean of the source over the footprint around `uv`, so a smaller stack does not alias. Each
// linear tap averages up to two texels per axis, so half as many taps as texels, at most 8 x 8.
@fragment
fn fs_push(v: V) -> @location(0) vec4f {
    let n = vec2i(clamp(ceil(push.footprint * 0.5), vec2f(1.0), vec2f(8.0)));
    var sum = vec4f(0.0);
    for (var j = 0; j < n.y; j++) {
        for (var i = 0; i < n.x; i++) {
            let o = (vec2f(f32(i), f32(j)) + 0.5) / vec2f(n) - 0.5;
            sum += textureSampleLevel(src, samp, v.uv + o * push.footprint * push.src_texel, 0.0);
        }
    }
    return sum / f32(n.x * n.y);
}
