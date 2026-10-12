// tex.chromakey: pixels whose chroma lies near the key colour's become transparent. Chroma is
// Cb/Cr (Rec. 709) of gamma-encoded colour, so distances follow the eye rather than linear light;
// brightness does not matter, which keeps shadows on the screen keyed. Spill pulls the key
// colour's tint out of what stays.

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
    var ycc = pn_ycc(c.rgb);
    let d = distance(ycc.yz, key);
    var keep = smoothstep(params.tolerance, params.tolerance + max(params.softness, 1e-4), d);
    if (params.invert != 0u) {
        keep = 1.0 - keep;
    }
    // Spill: remove the part of the chroma that points towards the key.
    let kl = length(key);
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
