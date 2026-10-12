// Shapes as a mask, a signed distance field or outlines (crates/gpu/src/shapes.rs). Every
// shape is a closed polygon in target pixels; each pixel finds its signed distance to the
// nearest one by walking their edges, skipping shapes whose bounding box is farther away than
// the best distance so far.

struct U {
    size: vec2f,
    mode: u32,
    count: u32,
    // Outline width in pixels, and what one pixel of distance is in the SDF's unit.
    width: f32,
    unit: f32,
    _p0: f32,
    _p1: f32,
}

struct Poly {
    // min x, min y, max x, max y in pixels
    bounds: vec4f,
    first: u32,
    n: u32,
    _p0: u32,
    _p1: u32,
    color: vec4f,
}

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var<storage, read> polys: array<Poly>;
@group(0) @binding(2) var<storage, read> points: array<vec2f>;

const MODE_MASK: u32 = 0u;
const MODE_SDF: u32 = 1u;
const MODE_OUTLINE: u32 = 2u;
const FAR: f32 = 1e9;
// Largest |distance| written, in the SDF's unit (16-bit float textures end at 65504).
const SDF_LIMIT: f32 = 1000.0;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    return vec4f(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) pos: vec4f) -> @location(0) vec4f {
    // Pixel centres are at .5.
    let p = pos.xy;
    // Signed distance to the union (negative inside) and the direction it grows in.
    var best = FAR;
    var grad = vec2f(0.0);
    // Unsigned distance to the nearest edge of any shape, and that shape's colour (outlines).
    var near = FAR;
    var near_color = vec4f(0.0);
    for (var i = 0u; i < u.count; i++) {
        let poly = polys[i];
        let lo = poly.bounds.xy;
        let hi = poly.bounds.zw;
        let outside = max(max(lo - p, p - hi), vec2f(0.0));
        let lower = length(outside);
        let bound = select(best, near, u.mode == MODE_OUTLINE);
        if (lower > 0.0 && lower >= bound) {
            continue;
        }
        var d2 = FAR;
        var closest = p;
        var inside = false;
        let n = poly.n;
        for (var k = 0u; k < n; k++) {
            let a = points[poly.first + k];
            let b = points[poly.first + select(k + 1u, 0u, k + 1u == n)];
            let e = b - a;
            let len2 = dot(e, e);
            var t = 0.0;
            if (len2 > 0.0) {
                t = clamp(dot(p - a, e) / len2, 0.0, 1.0);
            }
            let c = a + e * t;
            let q = p - c;
            let dd = dot(q, q);
            if (dd < d2) {
                d2 = dd;
                closest = c;
            }
            if ((a.y > p.y) != (b.y > p.y)) {
                let x = a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x);
                if (p.x < x) {
                    inside = !inside;
                }
            }
        }
        let d = sqrt(d2);
        if (d < near) {
            near = d;
            near_color = poly.color;
        }
        let sd = select(d, -d, inside);
        if (sd < best) {
            best = sd;
            var dir = vec2f(0.0);
            if (d > 1e-6) {
                dir = (p - closest) / d;
            }
            grad = select(dir, -dir, inside);
        }
    }
    switch u.mode {
        case MODE_SDF: {
            return vec4f(clamp(best * u.unit, -SDF_LIMIT, SDF_LIMIT), grad, 1.0);
        }
        case MODE_OUTLINE: {
            let a = clamp(u.width * 0.5 + 0.5 - near, 0.0, 1.0) * near_color.a;
            return vec4f(near_color.rgb * a, a);
        }
        default: {
            let c = clamp(0.5 - best, 0.0, 1.0);
            return vec4f(c);
        }
    }
}
