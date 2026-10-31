// tex.transform: translate, rotate and scale the input around a pivot, in the order `order`
// names. Units: uv (1 = the whole width or height), degrees counter-clockwise on screen.

const step_scale: i32 = 0;
const step_rotate: i32 = 1;
const step_translate: i32 = 2;

// The three steps in the order they act on the picture.
fn steps() -> vec3i {
    switch params.order {
        case order_str: { return vec3i(step_scale, step_translate, step_rotate); }
        case order_rst: { return vec3i(step_rotate, step_scale, step_translate); }
        case order_rts: { return vec3i(step_rotate, step_translate, step_scale); }
        case order_tsr: { return vec3i(step_translate, step_scale, step_rotate); }
        case order_trs: { return vec3i(step_translate, step_rotate, step_scale); }
        default: { return vec3i(step_scale, step_rotate, step_translate); }
    }
}

// One step undone, in square units around the pivot.
fn undo(step: i32, q: vec2f, aspect: vec2f) -> vec2f {
    switch step {
        case step_scale: {
            return q / params.scale;
        }
        case step_rotate: {
            let a = radians(params.rotate);
            let c = cos(a);
            let s = sin(a);
            // y points down, so a counter-clockwise turn on screen is clockwise in uv.
            return vec2f(c * q.x - s * q.y, s * q.x + c * q.y);
        }
        default: {
            return q - params.translate * aspect;
        }
    }
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let aspect = vec2f(pn.resolution.x / pn.resolution.y, 1.0);
    let sc = params.scale;
    if (abs(sc.x) < 1e-6 || abs(sc.y) < 1e-6) {
        return vec4f(0.0);
    }
    // Inverse mapping: from the output pixel back to where it comes from, the last step first.
    let order = steps();
    var q = (v.uv - params.pivot) * aspect;
    q = undo(order.z, q, aspect);
    q = undo(order.y, q, aspect);
    q = undo(order.x, q, aspect);
    let uv = q / aspect + params.pivot;
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
