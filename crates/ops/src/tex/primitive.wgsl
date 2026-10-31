// tex.primitive: one shape from its signed distance (negative inside), in image heights in the
// shape's own frame: centred, turned with it. Rectangle and polygon distances are exact; the
// ellipse's comes from its nearest point, found by a few refining steps.

const far: f32 = 1e9;

fn rectangle(p: vec2f, half: vec2f, corner: f32) -> f32 {
    let r = clamp(corner, 0.0, min(half.x, half.y));
    let q = abs(p) - half + r;
    return length(max(q, vec2f(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

// The nearest point on the ellipse by refining a guess on its evolute (after 0xfaded's method),
// in the first quadrant, then the distance to it with the side of the edge.
fn ellipse(p_in: vec2f, half: vec2f) -> f32 {
    let p = abs(p_in);
    let a = half.x;
    let b = half.y;
    var t = vec2f(0.70710678);
    for (var i = 0; i < 4; i++) {
        let xy = vec2f(a, b) * t;
        let e = vec2f((a * a - b * b) * t.x * t.x * t.x / a, (b * b - a * a) * t.y * t.y * t.y / b);
        let r = length(xy - e);
        let q = max(length(p - e), 1e-12);
        t = clamp(((p - e) * r / q + e) / vec2f(a, b), vec2f(0.0), vec2f(1.0));
        t = t / max(length(t), 1e-12);
    }
    let nearest = vec2f(a, b) * t;
    let k = (p.x / a) * (p.x / a) + (p.y / b) * (p.y / b);
    return length(p - nearest) * select(1.0, -1.0, k < 1.0);
}

fn corner_of(k: i32, n: i32, half: vec2f) -> vec2f {
    // y points down, so -90 degrees is up.
    let a = -1.5707963 + 6.2831853 * f32(k) / f32(n);
    return half * vec2f(cos(a), sin(a));
}

// The exact distance to a closed polygon, its sign by the crossing count (after Inigo Quilez).
fn polygon(p: vec2f, half: vec2f, sides: i32) -> f32 {
    let n = clamp(sides, 3, 64);
    var d = far;
    var s = 1.0;
    var prev = corner_of(n - 1, n, half);
    for (var k = 0; k < n; k++) {
        let v = corner_of(k, n, half);
        let e = prev - v;
        let w = p - v;
        let b = w - e * clamp(dot(w, e) / max(dot(e, e), 1e-12), 0.0, 1.0);
        d = min(d, dot(b, b));
        let c1 = p.y >= v.y;
        let c2 = p.y < prev.y;
        let c3 = e.x * w.y > e.y * w.x;
        if ((c1 && c2 && c3) || (!c1 && !c2 && !c3)) {
            s = -s;
        }
        prev = v;
    }
    return s * sqrt(d);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let bg = vec4f(params.background.rgb * params.background.a, params.background.a);
    let half = params.size * 0.5;
    if (half.x < 1e-6 || half.y < 1e-6) {
        return bg;
    }
    let aspect = pn.resolution.x / pn.resolution.y;
    var q = (v.uv - params.center) * vec2f(aspect, 1.0);
    let a = radians(params.rotate);
    let c = cos(a);
    let s = sin(a);
    // y points down, so a counter-clockwise turn on screen is clockwise in uv.
    q = vec2f(c * q.x - s * q.y, s * q.x + c * q.y);
    var d = far;
    switch params.shape {
        case shape_ellipse: {
            d = ellipse(q, half);
        }
        case shape_polygon: {
            d = polygon(q, half, params.sides);
        }
        default: {
            d = rectangle(q, half, params.corner);
        }
    }
    // In pixels; the edge fades over softness pixels, at least one.
    let px = d * pn.resolution.y;
    let w = max(params.softness, 1.0);
    let shape = clamp(0.5 - px / w, 0.0, 1.0);
    let inner = clamp(0.5 - (px + max(params.border, 0.0)) / w, 0.0, 1.0);
    // The fill covers the inner part, the border the band between; neither lies over the other.
    let fill = params.fill.a * inner;
    let band = params.border_color.a * max(shape - inner, 0.0);
    let rgb = params.fill.rgb * fill + params.border_color.rgb * band + bg.rgb * (1.0 - fill - band);
    return vec4f(rgb, fill + band + bg.a * (1.0 - fill - band));
}
