// tex.ramp: the lookup row in in0 (the node fills it from the keys) spread over the picture.

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = v.uv - 0.5;
    var t: f32;
    switch params.shape {
        case shape_vertical: {
            t = 1.0 - v.uv.y;
        }
        case shape_radial: {
            t = length(p * vec2f(pn.resolution.x / pn.resolution.y, 1.0)) * 2.0;
        }
        case shape_circular: {
            t = fract(atan2(-p.y, p.x) / 6.2831853);
        }
        default: {
            t = v.uv.x;
        }
    }
    let n = f32(textureDimensions(in0).x);
    let u = (clamp(t, 0.0, 1.0) * (n - 1.0) + 0.5) / n;
    return textureSampleLevel(in0, pn_clamp, vec2f(u, 0.5), 0.0);
}
