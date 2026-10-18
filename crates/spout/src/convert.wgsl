// Between penumbra's textures (linear light, premultiplied alpha, 16-bit float) and what Spout
// programs exchange: sRGB-encoded colour, 8 bits or float, alpha straight or premultiplied (on
// the encoded colour), or no alpha at all. Source and target have the same size; every pixel is
// loaded, not sampled, so any source format works.

// 0: penumbra to Spout (encode), 1: Spout to penumbra (decode).
override DIRECTION: u32 = 0u;
// 0: straight alpha, 1: premultiplied, 2: opaque (sending: over black; receiving: alpha ignored).
override ALPHA: u32 = 0u;
// Sending only, 1: clamp to 0..1 (an 8-bit target), 0: keep values above 1 (float).
override CLAMP: u32 = 1u;

@group(0) @binding(0) var src: texture_2d<f32>;

struct Vertex {
    @builtin(position) pos: vec4f,
}

@vertex
fn vs(@builtin(vertex_index) i: u32) -> Vertex {
    let uv = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    return Vertex(vec4f(uv * 2.0 - 1.0, 0.0, 1.0));
}

// NaN and infinities as 0, by their bits (a comparison may be optimised away).
fn clean(v: f32) -> f32 {
    if ((bitcast<u32>(v) & 0x7f800000u) == 0x7f800000u) {
        return 0.0;
    }
    return v;
}

fn oetf(v: f32) -> f32 {
    if (v <= 0.0031308) {
        return v * 12.92;
    }
    return 1.055 * pow(v, 1.0 / 2.4) - 0.055;
}

fn eotf(v: f32) -> f32 {
    if (v <= 0.04045) {
        return v / 12.92;
    }
    return pow((v + 0.055) / 1.055, 2.4);
}

fn encode(rgb: vec3f) -> vec3f {
    let c = max(rgb, vec3f(0.0));
    return vec3f(oetf(c.r), oetf(c.g), oetf(c.b));
}

fn decode(rgb: vec3f) -> vec3f {
    let c = max(rgb, vec3f(0.0));
    return vec3f(eotf(c.r), eotf(c.g), eotf(c.b));
}

@fragment
fn fs(v: Vertex) -> @location(0) vec4f {
    let s = textureLoad(src, vec2i(v.pos.xy), 0);
    var rgb = vec3f(clean(s.r), clean(s.g), clean(s.b));
    let a = clamp(clean(s.a), 0.0, 1.0);
    if (DIRECTION == 0u) {
        // Premultiplied linear in.
        var out = vec4f(0.0, 0.0, 0.0, 1.0);
        if (ALPHA == 2u) {
            out = vec4f(encode(rgb), 1.0);
        } else {
            let straight = select(vec3f(0.0), rgb / max(a, 1e-6), a > 0.0);
            var e = encode(straight);
            if (ALPHA == 1u) {
                e = e * a;
            }
            out = vec4f(e, a);
        }
        if (CLAMP == 1u) {
            out = clamp(out, vec4f(0.0), vec4f(1.0));
        }
        return out;
    }
    // Premultiplied linear out.
    if (ALPHA == 2u) {
        return vec4f(decode(rgb), 1.0);
    }
    if (ALPHA == 1u) {
        let straight = select(vec3f(0.0), rgb / max(a, 1e-6), a > 0.0);
        return vec4f(decode(straight) * a, a);
    }
    return vec4f(decode(rgb) * a, a);
}
