// tex.composite: `layer2` (in1) onto `layer1` (in0), premultiplied; `swap` exchanges them; the
// fold lays `layer3` and `layer4` on in turn. Fading a layer is tex.level's opacity, not a param here.

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
        case op_inside: {
            return b * a.a;
        }
        case op_outside: {
            return b * (1.0 - a.a);
        }
        case op_overlay: {
            // The separable blend over straight colours, where both are; each alone elsewhere.
            let x = pn_unpremultiply(a).rgb;
            let y = pn_unpremultiply(b).rgb;
            let o = select(1.0 - 2.0 * (1.0 - x) * (1.0 - y), 2.0 * x * y, x < vec3f(0.5));
            let rgb = o * a.a * b.a + y * b.a * (1.0 - a.a) + x * a.a * (1.0 - b.a);
            return vec4f(rgb, b.a + a.a * (1.0 - b.a));
        }
        case op_color: {
            return non_separable(pn_unpremultiply(a).rgb, a.a, pn_unpremultiply(b).rgb, b.a, false);
        }
        case op_luminosity: {
            return non_separable(pn_unpremultiply(a).rgb, a.a, pn_unpremultiply(b).rgb, b.a, true);
        }
        case op_atop: {
            return b * a.a + a * (1.0 - b.a);
        }
        default: {
            return b + a * (1.0 - b.a);
        }
    }
}

fn lum(c: vec3f) -> f32 {
    return dot(c, vec3f(0.3, 0.59, 0.11));
}

// `c` moved to luminance `l`, brought back into 0 to 1 without changing its hue (the W3C
// compositing spec's SetLum and ClipColor). Values above 1 (HDR) are left as they are.
fn set_lum(c: vec3f, l: f32) -> vec3f {
    let d = c + (l - lum(c));
    let m = lum(d);
    let lo = min(d.r, min(d.g, d.b));
    let hi = max(d.r, max(d.g, d.b));
    var o = d;
    if lo < 0.0 && m - lo > 1e-6 {
        o = m + (o - m) * m / (m - lo);
    }
    if hi > 1.0 && m <= 1.0 && hi - m > 1e-6 {
        o = m + (o - m) * (1.0 - m) / (hi - m);
    }
    return o;
}

// color (or luminosity with `swap_roles`): the top's colour at the bottom's luminance where both
// are, each alone elsewhere; straight colours `x` (bottom) and `y` (top) in, premultiplied out.
fn non_separable(x: vec3f, xa: f32, y: vec3f, ya: f32, swap_roles: bool) -> vec4f {
    let o = select(set_lum(y, lum(x)), set_lum(x, lum(y)), swap_roles);
    let rgb = o * xa * ya + y * ya * (1.0 - xa) + x * xa * (1.0 - ya);
    return vec4f(rgb, ya + xa * (1.0 - ya));
}
