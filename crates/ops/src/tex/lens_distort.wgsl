// tex.lens_distort: radial lens distortion (Brown-Conrady, k1 and k2) around `center`. The radius
// is 1 at half the image height, so the image's aspect ratio does not change the curve. Positive
// k1 pulls the edges in (barrel), negative pushes them out (pincushion); `scale` zooms after.

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let aspect = vec2f(pn.resolution.x / pn.resolution.y, 1.0);
    let p = (v.uv - params.center) * aspect * 2.0;
    let r2 = dot(p, p);
    let f = 1.0 + params.k1 * r2 + params.k2 * r2 * r2;
    let s = max(params.scale, 1e-4);
    let uv = p * f / s / (aspect * 2.0) + params.center;
    return pn_extend(in0, uv, params.extend);
}
