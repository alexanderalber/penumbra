// penumbra's texture (linear light, premultiplied alpha) to the bytes NDI sends: UYVY, video
// range, the matrix of the picture's size, sRGB-encoded colour; for UYVA followed by a plane of
// straight, full-range alpha bytes. One invocation writes one 32-bit word of the buffer: a pair
// of pixels of UYVY, or four alpha bytes.

// 0: straight alpha, 1: premultiplied (on the encoded colour), 2: none (over black).
override ALPHA: u32 = 0u;
override KR: f32 = 0.2126;
override KB: f32 = 0.0722;

struct Params {
    // Even; an odd texture's last column is left out.
    width: u32,
    height: u32,
    // UYVY words (width / 2 * height), then the alpha plane's words (0 without alpha).
    color_words: u32,
    total_words: u32,
    // Invocations per row of the dispatch grid.
    row: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var<storage, read_write> out: array<u32>;

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

fn encode(rgb: vec3f) -> vec3f {
    let c = clamp(rgb, vec3f(0.0), vec3f(1.0));
    return vec3f(oetf(c.r), oetf(c.g), oetf(c.b));
}

fn texel(x: u32, y: u32) -> vec4f {
    let s = textureLoad(src, vec2u(x, y), 0);
    return vec4f(clean(s.r), clean(s.g), clean(s.b), clamp(clean(s.a), 0.0, 1.0));
}

// R'G'B' as sent, by the alpha mode.
fn encoded(s: vec4f) -> vec3f {
    if (ALPHA == 2u) {
        return encode(s.rgb);
    }
    let straight = select(vec3f(0.0), s.rgb / max(s.a, 1e-6), s.a > 0.0);
    let e = encode(straight);
    if (ALPHA == 1u) {
        return e * s.a;
    }
    return e;
}

fn byte(v: f32) -> u32 {
    return u32(clamp(round(v), 0.0, 255.0));
}

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3u) {
    let i = id.y * params.row + id.x;
    if (i >= params.total_words) {
        return;
    }
    if (i < params.color_words) {
        let pairs = params.width / 2u;
        let x = (i % pairs) * 2u;
        let y = i / pairs;
        let c0 = encoded(texel(x, y));
        let c1 = encoded(texel(x + 1u, y));
        let k = vec3f(KR, 1.0 - KR - KB, KB);
        let y0 = dot(c0, k);
        let y1 = dot(c1, k);
        let c = (c0 + c1) * 0.5;
        let yc = (y0 + y1) * 0.5;
        let cb = (c.b - yc) / (2.0 * (1.0 - KB));
        let cr = (c.r - yc) / (2.0 * (1.0 - KR));
        out[i] = byte(128.0 + 224.0 * cb)
            | (byte(16.0 + 219.0 * y0) << 8u)
            | (byte(128.0 + 224.0 * cr) << 16u)
            | (byte(16.0 + 219.0 * y1) << 24u);
        return;
    }
    // Four alpha bytes, row after row without gaps.
    let first = (i - params.color_words) * 4u;
    let pixels = params.width * params.height;
    var word = 0u;
    for (var k = 0u; k < 4u; k++) {
        let p = first + k;
        if (p < pixels) {
            let a = texel(p % params.width, p / params.width).a;
            word = word | (byte(a * 255.0) << (8u * k));
        }
    }
    out[i] = word;
}
