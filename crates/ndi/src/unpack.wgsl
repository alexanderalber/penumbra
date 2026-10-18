// A received NDI picture to penumbra's texture (linear light, premultiplied alpha, 16-bit
// float). The bytes come as they arrived, in an 8-bit RGBA texture: UYVY as one texel per pair
// of pixels (U, Y0, V, Y1), BGRA and RGBA as one texel per pixel. UYVA's alpha is a second,
// one-byte texture. NDI's Y'CbCr is video range with the matrix of the picture's size; its
// alpha is straight and full range, its colour sRGB-encoded as every program shows it.

// 0 UYVY, 1 UYVA, 2 BGRA, 3 BGRX, 4 RGBA, 5 RGBX.
override MODE: u32 = 0u;
// 0: straight alpha (NDI's), 1: premultiplied, 2: ignored.
override ALPHA: u32 = 0u;
// Luma coefficients of the matrix.
override KR: f32 = 0.2126;
override KB: f32 = 0.0722;
// 1: the picture is one field, each line shown twice.
override FIELD: u32 = 0u;

@group(0) @binding(0) var color: texture_2d<f32>;
@group(0) @binding(1) var alpha_plane: texture_2d<f32>;

struct Vertex {
    @builtin(position) pos: vec4f,
}

@vertex
fn vs(@builtin(vertex_index) i: u32) -> Vertex {
    let uv = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    return Vertex(vec4f(uv * 2.0 - 1.0, 0.0, 1.0));
}

fn eotf(v: f32) -> f32 {
    if (v <= 0.04045) {
        return v / 12.92;
    }
    return pow((v + 0.055) / 1.055, 2.4);
}

fn decode(rgb: vec3f) -> vec3f {
    let c = clamp(rgb, vec3f(0.0), vec3f(1.0));
    return vec3f(eotf(c.r), eotf(c.g), eotf(c.b));
}

// Video-range Y'CbCr bytes (as 0..1) to R'G'B'.
fn rgb_of(y: f32, cb: f32, cr: f32) -> vec3f {
    let l = (y * 255.0 - 16.0) / 219.0;
    let u = (cb * 255.0 - 128.0) / 224.0;
    let v = (cr * 255.0 - 128.0) / 224.0;
    let r = l + 2.0 * (1.0 - KR) * v;
    let b = l + 2.0 * (1.0 - KB) * u;
    let g = (l - KR * r - KB * b) / (1.0 - KR - KB);
    return vec3f(r, g, b);
}

@fragment
fn fs(v: Vertex) -> @location(0) vec4f {
    var p = vec2i(v.pos.xy);
    if (FIELD == 1u) {
        p.y = p.y / 2;
    }
    var rgb = vec3f(0.0);
    var a = 1.0;
    if (MODE <= 1u) {
        let pairs = i32(textureDimensions(color).x);
        let m = p.x / 2;
        let t = textureLoad(color, vec2i(m, p.y), 0);
        var luma = t.g;
        var chroma = vec2f(t.r, t.b);
        if ((p.x & 1) == 1) {
            luma = t.a;
            // Between this pair's chroma and the next one's.
            let n = textureLoad(color, vec2i(min(m + 1, pairs - 1), p.y), 0);
            chroma = (chroma + vec2f(n.r, n.b)) * 0.5;
        }
        rgb = rgb_of(luma, chroma.x, chroma.y);
        if (MODE == 1u) {
            a = textureLoad(alpha_plane, p, 0).r;
        }
    } else {
        let t = textureLoad(color, p, 0);
        if (MODE <= 3u) {
            rgb = t.bgr;
        } else {
            rgb = t.rgb;
        }
        if (MODE == 2u || MODE == 4u) {
            a = t.a;
        }
    }
    if (ALPHA == 2u) {
        return vec4f(decode(rgb), 1.0);
    }
    if (ALPHA == 1u) {
        let straight = select(vec3f(0.0), rgb / max(a, 1e-6), a > 0.0);
        return vec4f(decode(straight) * a, a);
    }
    return vec4f(decode(rgb) * a, a);
}
