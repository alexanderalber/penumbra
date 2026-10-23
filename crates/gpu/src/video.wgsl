// Decoded video planes to a `tex` texture: linear light, premultiplied alpha.
//
// Planes are unsigned integer textures read with textureLoad (8- and 16-bit alike, so one
// pipeline serves every format and no 16-bit normalised format feature is needed). Chroma is
// interpolated by hand at the MPEG siting: co-sited with the left luma sample horizontally,
// between the two luma rows vertically for 4:2:0.

struct Conv {
    size: vec2u,
    // Chroma subsampling as shifts: (1, 1) 4:2:0, (1, 0) 4:2:2, (0, 0) 4:4:4.
    shift: vec2u,
    // 0: Y'CbCr planes, 1: sRGB-encoded RGBA in the first texture.
    mode: u32,
    bits: u32,
    full_range: u32,
    // 0: sRGB / BT.709, 1: linear, 2: PQ (1.0 = 100 nits), 3: HLG.
    transfer: u32,
    kr: f32,
    kb: f32,
    has_alpha: u32,
    _pad: u32,
}

@group(0) @binding(0) var<uniform> c: Conv;
@group(0) @binding(1) var plane_y: texture_2d<u32>;
@group(0) @binding(2) var plane_cb: texture_2d<u32>;
@group(0) @binding(3) var plane_cr: texture_2d<u32>;
@group(0) @binding(4) var plane_a: texture_2d<u32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    return pn_fullscreen(i);
}

fn load(t: texture_2d<u32>, p: vec2i) -> f32 {
    let size = vec2i(textureDimensions(t));
    return f32(textureLoad(t, clamp(p, vec2i(0), size - 1), 0).x);
}

// Bilinear chroma at a fractional chroma-sample position.
fn chroma(t: texture_2d<u32>, f: vec2f) -> f32 {
    let base = floor(f);
    let w = f - base;
    let p = vec2i(base);
    let a = mix(load(t, p), load(t, p + vec2i(1, 0)), w.x);
    let b = mix(load(t, p + vec2i(0, 1)), load(t, p + vec2i(1, 1)), w.x);
    return mix(a, b, w.y);
}

fn srgb_to_linear(v: f32) -> f32 {
    if (v <= 0.04045) {
        return v / 12.92;
    }
    return pow((v + 0.055) / 1.055, 2.4);
}

fn pq_to_linear(v: f32) -> f32 {
    let m1 = 0.1593017578125;
    let m2 = 78.84375;
    let c1 = 0.8359375;
    let c2 = 18.8515625;
    let c3 = 18.6875;
    let p = pow(max(v, 0.0), 1.0 / m2);
    // 10 000 nits at 1.0; scaled so 100 nits (SDR reference white) is 1.0.
    return pow(max(p - c1, 0.0) / (c2 - c3 * p), 1.0 / m1) * 100.0;
}

fn hlg_to_linear(v: f32) -> f32 {
    let a = 0.17883277;
    let b = 0.28466892;
    let cc = 0.55991073;
    if (v <= 0.5) {
        return v * v / 3.0;
    }
    return (exp((v - cc) / a) + b) / 12.0;
}

fn to_linear(v: f32) -> f32 {
    switch c.transfer {
        case 1u: { return v; }
        case 2u: { return pq_to_linear(v); }
        case 3u: { return hlg_to_linear(v); }
        default: { return srgb_to_linear(clamp(v, 0.0, 1.0)); }
    }
}

@fragment
fn fs(@builtin(position) pos: vec4f) -> @location(0) vec4f {
    let p = vec2i(floor(pos.xy));
    if (c.mode == 1u) {
        let s = vec4f(textureLoad(plane_y, p, 0)) / 255.0;
        let rgb = vec3f(srgb_to_linear(s.r), srgb_to_linear(s.g), srgb_to_linear(s.b));
        return vec4f(rgb * s.a, s.a);
    }
    let scale = f32(1u << (c.bits - 8u));
    let top = f32((1u << c.bits) - 1u);
    let sub = vec2f(f32(1u << c.shift.x), f32(1u << c.shift.y));
    // Left siting horizontally; vertically centred between the rows it covers.
    let fx = f32(p.x) / sub.x;
    let fy = (f32(p.y) + 0.5) / sub.y - 0.5;
    let yv = load(plane_y, p);
    let cb = chroma(plane_cb, vec2f(fx, fy));
    let cr = chroma(plane_cr, vec2f(fx, fy));
    var y: f32;
    var u: f32;
    var v: f32;
    if (c.full_range == 1u) {
        let mid = f32(1u << (c.bits - 1u));
        y = yv / top;
        u = (cb - mid) / top;
        v = (cr - mid) / top;
    } else {
        y = (yv - 16.0 * scale) / (219.0 * scale);
        u = (cb - 128.0 * scale) / (224.0 * scale);
        v = (cr - 128.0 * scale) / (224.0 * scale);
    }
    let kg = 1.0 - c.kr - c.kb;
    let r = y + 2.0 * (1.0 - c.kr) * v;
    let b = y + 2.0 * (1.0 - c.kb) * u;
    let g = (y - c.kr * r - c.kb * b) / kg;
    let rgb = vec3f(to_linear(r), to_linear(g), to_linear(b));
    var a = 1.0;
    if (c.has_alpha == 1u) {
        a = clamp(load(plane_a, p) / top, 0.0, 1.0);
    }
    return vec4f(max(rgb, vec3f(0.0)) * a, a);
}
