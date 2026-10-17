// Frame stacks (stack.rs): per pixel, a map picks how many frames back to look.

struct Read {
    // Frames back at a map value of 1.
    reach: f32,
    // Frames back added to every pixel.
    offset: f32,
    // The layer holding the newest frame, counted over all chunks.
    head: u32,
    // Frames written so far (the oldest is `count - 1` back), and room for.
    count: u32,
    frames: u32,
    // Layers per chunk.
    chunk: u32,
    // 1: mix the two frames around the map's value; 0: the nearer one.
    blend: u32,
    pad: u32,
};

@group(0) @binding(0) var map: texture_2d<f32>;
@group(0) @binding(1) var<uniform> look: Read;
// The chunks (c0, c1, ...), the sampler and layer(), which picks layer k counted over all
// chunks, are appended by stack.rs for as many chunks as the device can bind.

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

// The frame `back` frames before the newest.
fn frame(back: u32, uv: vec2f) -> vec4f {
    return layer((look.head + look.frames - back % look.frames) % look.frames, uv);
}

@fragment
fn fs_read(v: V) -> @location(0) vec4f {
    let m = textureSampleLevel(map, samp, v.uv, 0.0);
    var back = look.offset + dot(m.rgb, vec3f(0.2126, 0.7152, 0.0722)) * look.reach;
    // NaN fails every comparison and lands on the newest frame.
    let oldest = f32(max(look.count, 1u) - 1u);
    if !(back > 0.0) {
        back = 0.0;
    }
    back = min(back, oldest);
    let near = u32(floor(back));
    let t = back - floor(back);
    if look.blend == 0u {
        return frame(u32(round(back)), v.uv);
    }
    let a = frame(near, v.uv);
    if t <= 0.0 {
        return a;
    }
    return mix(a, frame(min(near + 1u, u32(oldest)), v.uv), t);
}
