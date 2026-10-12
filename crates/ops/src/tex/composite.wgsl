// tex.composite: `top` (in1) onto `bottom` (in0), premultiplied; `swap` exchanges them. Fading
// the top is tex.level's opacity, not a param here.

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let lower = textureSampleLevel(in0, pn_clamp, v.uv, 0.0);
    let upper = textureSampleLevel(in1, pn_clamp, v.uv, 0.0);
    let a = select(lower, upper, params.swap != 0u);
    let b = select(upper, lower, params.swap != 0u);
    switch params.op {
        case op_add: {
            return a + b;
        }
        case op_multiply: {
            return a * b;
        }
        case op_screen: {
            return a + b - a * b;
        }
        case op_subtract: {
            return vec4f(a.rgb - b.rgb, a.a);
        }
        case op_difference: {
            return vec4f(abs(a.rgb - b.rgb), max(a.a, b.a));
        }
        case op_maximum: {
            return max(a, b);
        }
        case op_minimum: {
            return min(a, b);
        }
        case op_average: {
            return (a + b) * 0.5;
        }
        default: {
            return b + a * (1.0 - b.a);
        }
    }
}
