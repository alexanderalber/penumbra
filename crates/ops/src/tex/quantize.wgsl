// tex.quantize: fewer colours and, with `block` above 1, fewer pixels. Each channel snaps to
// `levels` evenly spaced values, even to the eye (sRGB curve) or in linear light; a block of
// pixels takes the colour at its centre. Ordered dithering moves the thresholds per block in a
// 4 x 4 Bayer pattern, so gradients keep their shape with few levels.

fn to_srgb(c: vec3f) -> vec3f {
    let x = max(c, vec3f(0.0));
    return select(1.055 * pow(x, vec3f(1.0 / 2.4)) - 0.055, x * 12.92, x <= vec3f(0.0031308));
}

fn from_srgb(c: vec3f) -> vec3f {
    let x = max(c, vec3f(0.0));
    return select(pow((x + 0.055) / 1.055, vec3f(2.4)), x / 12.92, x <= vec3f(0.04045));
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let size = vec2i(textureDimensions(in0));
    let b = max(params.block, 1);
    let cell = vec2i(floor(v.pos.xy)) / b;
    let p = clamp(cell * b + b / 2, vec2i(0), size - 1);
    let c = pn_unpremultiply(textureLoad(in0, p, 0));
    let n = f32(max(params.levels, 2) - 1);
    // The threshold offset in steps: 0 rounds to the nearest level.
    var d = 0.0;
    if (params.dither == dither_ordered) {
        var bayer = array<f32, 16>(0.0, 8.0, 2.0, 10.0, 12.0, 4.0, 14.0, 6.0, 3.0, 11.0, 1.0, 9.0, 15.0, 7.0, 13.0, 5.0);
        let k = u32(cell.x & 3) + 4u * u32(cell.y & 3);
        d = (bayer[k] + 0.5) / 16.0 - 0.5;
    }
    var rgb = c.rgb;
    if (params.space == space_perceptual) {
        rgb = to_srgb(rgb);
    }
    rgb = max(floor(rgb * n + 0.5 + d), vec3f(0.0)) / n;
    if (params.space == space_perceptual) {
        rgb = from_srgb(rgb);
    }
    var a = c.a;
    if (params.alpha != 0u) {
        a = clamp(floor(a * n + 0.5 + d) / n, 0.0, 1.0);
    }
    return vec4f(rgb * a, a);
}
