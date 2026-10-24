// Gives every face one colour of a small palette, keeping the pulse's brightness. in0: a picture
// whose brightness is kept (tex.shape_content's pulse), in1: a labels picture (red the face
// k / 255), in2: optional, white where faces may light (e.g. faces a projector reaches entirely);
// unconnected, all may. Face k takes colour (k * 3) mod count, so neighbouring numbers rarely
// share one. The default palette: pumpkin orange and its complement violet, split by a poison
// green and two darker steps (amber, plum), all at the same saturation.
// @param count int = 5 1..5 -- how many of the colours are used
// @param c0 color = [1.0, 0.42, 0.02, 1]
// @param c1 color = [0.42, 0.12, 0.85, 1]
// @param c2 color = [0.38, 0.85, 0.08, 1]
// @param c3 color = [0.95, 0.68, 0.05, 1]
// @param c4 color = [0.6, 0.05, 0.45, 1]
// @param palette palette -- a palette instead of c0 to c4 (a name, hex colours, or a number from a channel): all its colours are used, whatever count says; empty uses c0 to c4
// @param gain float = 1.3 0..4

fn colour(i: i32) -> vec3f {
    if (params.palette.count > 0) {
        return pn_palette(params.palette, i).rgb;
    }
    switch i {
        case 0: { return params.c0.rgb; }
        case 1: { return params.c1.rgb; }
        case 2: { return params.c2.rgb; }
        case 3: { return params.c3.rgb; }
        default: { return params.c4.rgb; }
    }
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = vec2i(v.pos.xy);
    let src = textureLoad(in0, p, 0);
    let face = i32(round(textureLoad(in1, p, 0).r * 255.0));
    // An unconnected in2 is a single transparent pixel: every face may light.
    let gated = textureDimensions(in2).x > 1u && textureLoad(in2, p, 0).r < 0.5;
    if (face <= 0 || src.a <= 0.0 || gated) {
        return vec4f(0.0);
    }
    let bright = max(src.r, max(src.g, src.b));
    let n = select(max(params.count, 1), params.palette.count, params.palette.count > 0);
    return vec4f(colour((face * 3) % n) * bright * params.gain, src.a);
}
