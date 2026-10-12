// tex.edge: Sobel edges of the luminance. `edges` draws them in `color` on black, `alpha` on
// transparent, `over` on top of the input.

fn pn_lum(uv: vec2f) -> f32 {
    let c = textureSampleLevel(in0, pn_clamp, uv, 0.0);
    return pn_luma(c.rgb);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let t = pn.texel * params.distance;
    let tl = pn_lum(v.uv + vec2f(-t.x, -t.y));
    let tc = pn_lum(v.uv + vec2f(0.0, -t.y));
    let tr = pn_lum(v.uv + vec2f(t.x, -t.y));
    let ml = pn_lum(v.uv + vec2f(-t.x, 0.0));
    let mr = pn_lum(v.uv + vec2f(t.x, 0.0));
    let bl = pn_lum(v.uv + vec2f(-t.x, t.y));
    let bc = pn_lum(v.uv + vec2f(0.0, t.y));
    let br = pn_lum(v.uv + vec2f(t.x, t.y));
    let gx = (tr + 2.0 * mr + br) - (tl + 2.0 * ml + bl);
    let gy = (bl + 2.0 * bc + br) - (tl + 2.0 * tc + tr);
    let e = clamp(length(vec2f(gx, gy)) * params.strength, 0.0, 1.0) * params.color.a;
    let edge = vec4f(params.color.rgb * e, e);
    switch params.mode {
        case mode_alpha: {
            return edge;
        }
        case mode_over: {
            let src = textureSampleLevel(in0, pn_clamp, v.uv, 0.0);
            return edge + src * (1.0 - e);
        }
        default: {
            return vec4f(edge.rgb, 1.0);
        }
    }
}
