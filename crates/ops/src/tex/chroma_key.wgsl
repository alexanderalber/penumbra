// tex.chroma_key: pixels whose colour lies near the key colour's become transparent. Colour is
// Cb/Cr (Rec. 709) of gamma-encoded colour, so distances follow the eye rather than linear light.
// Hue (the angle in the Cb/Cr plane) and saturation (the length there, relative to the key's) get
// separate tolerances: a shadow on the screen keeps the hue but loses saturation in proportion to
// its brightness, so a narrow hue window with a low saturation floor keys shadows without eating
// the neighbouring hues. Spill pulls the key colour's tint out of what stays.

fn pn_ycc(rgb: vec3f) -> vec3f {
    let e = pow(max(rgb, vec3f(0.0)), vec3f(1.0 / 2.2));
    let y = dot(e, vec3f(0.2126, 0.7152, 0.0722));
    return vec3f(y, (e.b - y) / 1.8556, (e.r - y) / 1.5748);
}

fn pn_rgb(ycc: vec3f) -> vec3f {
    let r = ycc.x + 1.5748 * ycc.z;
    let b = ycc.x + 1.8556 * ycc.y;
    let g = (ycc.x - 0.2126 * r - 0.0722 * b) / 0.7152;
    return pow(max(vec3f(r, g, b), vec3f(0.0)), vec3f(2.2));
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let c = pn_unpremultiply(textureSampleLevel(in0, pn_clamp, v.uv, 0.0));
    let key = pn_ycc(params.key.rgb).yz;
    let kl = length(key);
    var ycc = pn_ycc(c.rgb);
    let cl = length(ycc.yz);
    var keyed = 0.0;
    // A grey key has no hue to match; it keys nothing.
    if (kl > 1e-4) {
        // A grey pixel has no hue either; it counts as opposite the key.
        var cos_a = -1.0;
        if (cl > 1e-5) {
            cos_a = dot(ycc.yz, key) / (cl * kl);
        }
        let angle = degrees(acos(clamp(cos_a, -1.0, 1.0)));
        let hue = 1.0 - smoothstep(params.hue, params.hue + max(params.hue_softness, 1e-4), angle);
        let sat = smoothstep(
            params.saturation - max(params.saturation_softness, 1e-4),
            params.saturation,
            cl / kl,
        );
        let soft = max(params.brightness_softness, 1e-4);
        let bright = smoothstep(params.brightness.x - soft, params.brightness.x, ycc.x)
            * (1.0 - smoothstep(params.brightness.y, params.brightness.y + soft, ycc.x));
        keyed = hue * sat * bright;
    }
    var keep = 1.0 - keyed;
    if (params.invert != 0u) {
        keep = 1.0 - keep;
    }
    // Spill: remove the part of the chroma that points towards the key.
    if (kl > 1e-4) {
        let dir = key / kl;
        let s = max(dot(ycc.yz, dir), 0.0);
        ycc = vec3f(ycc.x, ycc.yz - dir * s * params.spill);
    }
    let a = c.a * keep;
    if (params.output == output_matte) {
        return vec4f(vec3f(a), 1.0);
    }
    return vec4f(pn_rgb(ycc) * a, a);
}
