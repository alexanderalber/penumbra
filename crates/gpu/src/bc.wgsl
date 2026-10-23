// Block-compressed (HAP) frames to a `tex` texture: linear light, premultiplied alpha.
//
// The BC textures are padded to whole 4x4 blocks; the target has the frame's own size, so the
// padding is never read. Colour in HAP is sRGB-encoded; Hap Q's scaled YCoCg is decoded as in
// Vidvox's reference shader (ScaledCoCgYToRGBA.frag) before the sRGB curve.

struct Conv {
    // 0: RGB (opaque), 1: RGBA (straight alpha), 2: scaled YCoCg, 3: alpha only.
    mode: u32,
    // Hap Q Alpha: alpha comes from the second texture's red channel.
    has_alpha: u32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<uniform> c: Conv;
@group(0) @binding(1) var tex_color: texture_2d<f32>;
@group(0) @binding(2) var tex_alpha: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    return pn_fullscreen(i);
}

fn srgb_to_linear(v: vec3f) -> vec3f {
    return pn_srgb_decode(clamp(v, vec3f(0.0), vec3f(1.0)));
}

@fragment
fn fs(@builtin(position) pos: vec4f) -> @location(0) vec4f {
    let p = vec2i(floor(pos.xy));
    let s = textureLoad(tex_color, p, 0);
    switch c.mode {
        case 1u: {
            return vec4f(srgb_to_linear(s.rgb) * s.a, s.a);
        }
        case 2u: {
            let half_ = 0.50196078431373;
            let scale = s.b * (255.0 / 8.0) + 1.0;
            let co = (s.r - half_) / scale;
            let cg = (s.g - half_) / scale;
            let y = s.a;
            let rgb = srgb_to_linear(vec3f(y + co - cg, y + cg, y - co - cg));
            var a = 1.0;
            if (c.has_alpha == 1u) {
                a = textureLoad(tex_alpha, p, 0).r;
            }
            return vec4f(rgb * a, a);
        }
        case 3u: {
            return vec4f(s.r);
        }
        default: {
            return vec4f(srgb_to_linear(s.rgb), 1.0);
        }
    }
}
