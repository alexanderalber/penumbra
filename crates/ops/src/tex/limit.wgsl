// tex.limit: keeps values between `min` and `max` on straight colour: clamped, wrapped around
// (a ramp becomes a sawtooth) or mirrored (a triangle wave). Alpha only when asked.

fn pn_limit(x: vec4f) -> vec4f {
    let lo = params.min;
    let span = max(params.max - params.min, 1e-6);
    switch params.mode {
        case mode_wrap: {
            let t = (x - lo) / span;
            return lo + (t - floor(t)) * span;
        }
        case mode_mirror: {
            let t = (x - lo) / span;
            let m = t - 2.0 * floor(t * 0.5);
            return lo + (1.0 - abs(m - 1.0)) * span;
        }
        default: {
            return clamp(x, vec4f(lo), vec4f(lo + span));
        }
    }
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let c = pn_unpremultiply(textureSampleLevel(in0, pn_clamp, v.uv, 0.0));
    let l = pn_limit(c);
    let a = select(c.a, clamp(l.a, 0.0, 1.0), params.alpha != 0u);
    return vec4f(l.rgb * a, a);
}
