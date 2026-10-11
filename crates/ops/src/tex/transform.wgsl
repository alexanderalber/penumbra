// tex.transform: translate, rotate and scale the input around a pivot. Units: uv (1 = the
// whole width or height), degrees counter-clockwise on screen.

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let aspect = vec2f(pn.resolution.x / pn.resolution.y, 1.0);
    let sc = params.scale;
    if (abs(sc.x) < 1e-6 || abs(sc.y) < 1e-6) {
        return vec4f(0.0);
    }
    // Inverse mapping: from the output pixel back to where it comes from, in square units.
    var q = (v.uv - params.pivot - params.translate) * aspect;
    let a = radians(params.rotate);
    let c = cos(a);
    let s = sin(a);
    // y points down, so a counter-clockwise turn on screen is clockwise in uv.
    q = vec2f(c * q.x - s * q.y, s * q.x + c * q.y);
    let uv = q / sc / aspect + params.pivot;
    switch params.extend {
        case extend_zero: {
            if (any(uv < vec2f(0.0)) || any(uv > vec2f(1.0))) {
                return vec4f(0.0);
            }
            return textureSampleLevel(in0, pn_clamp, uv, 0.0);
        }
        case extend_repeat: {
            return textureSampleLevel(in0, pn_repeat, uv, 0.0);
        }
        case extend_mirror: {
            return textureSampleLevel(in0, pn_mirror, uv, 0.0);
        }
        default: {
            return textureSampleLevel(in0, pn_clamp, uv, 0.0);
        }
    }
}
