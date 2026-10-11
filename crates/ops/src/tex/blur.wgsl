// tex.blur: one axis of a separable Gaussian (the node runs it twice, `axis` set by the node,
// not a user param). `size` is the radius in pixels, three standard deviations. Pairs of taps
// are merged into one linear sample between them, so a radius of r costs about r samples.

fn pn_gauss(x: f32, sigma: f32) -> f32 {
    return exp(-0.5 * x * x / (sigma * sigma));
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let radius = clamp(params.size, 0.0, 512.0);
    if (radius < 0.5 || all(params.axis == vec2f(0.0))) {
        return pn_extend(in0, v.uv, params.extend);
    }
    let sigma = max(radius / 3.0, 0.3);
    let stp = params.axis / vec2f(textureDimensions(in0));
    let n = i32(ceil(radius));
    var sum = pn_extend(in0, v.uv, params.extend);
    var total = 1.0;
    for (var i = 1; i <= n; i += 2) {
        let w1 = pn_gauss(f32(i), sigma);
        let w2 = select(0.0, pn_gauss(f32(i + 1), sigma), i + 1 <= n);
        let w = w1 + w2;
        let o = (f32(i) * w1 + f32(i + 1) * w2) / w;
        sum += w * (pn_extend(in0, v.uv + o * stp, params.extend) + pn_extend(in0, v.uv - o * stp, params.extend));
        total += 2.0 * w;
    }
    return sum / total;
}
