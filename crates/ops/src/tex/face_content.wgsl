// tex.face_content: in0 the picture delivered for a face, in1 the face's uv map, the output's
// size (red and green the face's u and v, 0 to 1; blue 1 on the face, 0.5 on an object in front
// of it, 0.25 on another face, 0 or -1 outside it). Each pixel on the face shows the picture at
// its u and v, fitted as `fit` says; the rest is transparent.

// u and v on the face to the picture's own 0 to 1, as `fit` says.
fn fitted(uv: vec2f) -> vec2f {
    let dims = vec2f(textureDimensions(in0));
    let r = (dims.x / max(dims.y, 1.0)) / max(params.aspect, 1e-4);
    var q = uv;
    if (params.fit == fit_contain) {
        if (r > 1.0) { q.y = (q.y - 0.5) * r + 0.5; } else { q.x = (q.x - 0.5) / r + 0.5; }
    } else if (params.fit == fit_cover) {
        if (r > 1.0) { q.x = (q.x - 0.5) / r + 0.5; } else { q.y = (q.y - 0.5) * r + 0.5; }
    }
    return q;
}

// Whether a texel of the map lies on the face (or on an object in front of it, when asked).
fn on(m: vec4f) -> bool {
    if (m.b > 0.9) {
        return true;
    }
    return params.objects != 0u && m.b > 0.4 && m.b < 0.6;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let size = vec2i(textureDimensions(in1));
    let p = vec2i(v.pos.xy);
    let m = textureLoad(in1, p, 0);
    if (!on(m)) {
        return vec4f(0.0);
    }
    let q = fitted(m.rg);
    if (any(q < vec2f(-0.001)) || any(q > vec2f(1.001))) {
        return vec4f(0.0);
    }
    // How many of the picture's texels one output pixel spans, from the neighbours on the face,
    // so a large picture on a small face does not alias.
    let dims = vec2f(textureDimensions(in0));
    var span = vec2f(1.0);
    for (var k = 0; k < 2; k++) {
        let step = select(vec2i(0, 1), vec2i(1, 0), k == 0);
        var n = textureLoad(in1, clamp(p + step, vec2i(0), size - 1), 0);
        if (!on(n)) {
            n = textureLoad(in1, clamp(p - step, vec2i(0), size - 1), 0);
        }
        if (on(n)) {
            span = max(span, abs(fitted(n.rg) - q) * dims);
        }
    }
    return pn_area(in0, pn_clamp, clamp(q, vec2f(0.0), vec2f(1.0)), min(span, vec2f(64.0)));
}
