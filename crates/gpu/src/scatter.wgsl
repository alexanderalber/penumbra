// Adds points into a picture (crates/gpu/src/scatter.rs). Every texel of `positions` is a point
// (red: x, green: y); it becomes one point primitive per pixel of its splat, placed on that
// pixel's centre, and the blend state adds its value there.

struct U {
    // What each point adds without a texture of values.
    value: vec4f,
    // The target's size in pixels.
    size: vec2f,
    // 0: pixels from the top left, 1: shares of the target.
    units: u32,
    // 0: drop, 1: wrap, 2: clamp.
    edge: u32,
    // Channel of `positions` that marks a dead point when below 0 (2: blue, 3: alpha); 0: none.
    dead: u32,
    // Side of the square of pixels per point.
    splat: u32,
    // Whether `values` holds the points' values.
    has_values: u32,
    _p0: u32,
}

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var positions: texture_2d<f32>;
@group(0) @binding(2) var values: texture_2d<f32>;

struct Out {
    @builtin(position) pos: vec4f,
    @location(0) @interpolate(flat) value: vec4f,
}

// Finite by the bits: a shader compiler may assume floats are never NaN and fold `x != x` away.
fn finite(x: f32) -> bool {
    return (bitcast<u32>(x) & 0x7f800000u) != 0x7f800000u;
}

fn finite4(v: vec4f) -> vec4f {
    return select(vec4f(0.0), v, vec4<bool>(finite(v.x), finite(v.y), finite(v.z), finite(v.w)));
}

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> Out {
    let k = max(u.splat, 1u);
    let i = vi / (k * k);
    let j = vi % (k * k);
    let dims = textureDimensions(positions);
    let t = vec2u(i % dims.x, i / dims.x);
    var ok = t.y < dims.y;
    let p = textureLoad(positions, min(t, dims - 1u), 0);
    ok = ok && finite(p.x) && finite(p.y);
    if u.dead == 2u {
        ok = ok && !(p.b < 0.0);
    } else if u.dead == 3u {
        ok = ok && !(p.a < 0.0);
    }
    var xy = p.xy;
    if u.units == 1u {
        xy = xy * u.size;
    }
    ok = ok && finite(xy.x) && finite(xy.y);
    // The pixel of this vertex: the point's, moved within the splat. Odd sides centre on it.
    let offset = vec2f(f32(j % k), f32(j / k)) - floor(f32(k - 1u) * 0.5);
    var cell = floor(xy) + offset;
    let last = u.size - 1.0;
    if u.edge == 1u {
        cell = cell - u.size * floor(cell / u.size);
        cell = clamp(cell, vec2f(0.0), last);
    } else if u.edge == 2u {
        cell = clamp(cell, vec2f(0.0), last);
    } else {
        ok = ok && all(cell >= vec2f(0.0)) && all(cell <= last);
    }
    var out: Out;
    // Outside the clip volume in x, y and depth: the point is not drawn.
    out.pos = vec4f(-2.0, -2.0, -2.0, 1.0);
    out.value = vec4f(0.0);
    if ok {
        let ndc = (cell + 0.5) / u.size * 2.0 - 1.0;
        out.pos = vec4f(ndc.x, -ndc.y, 0.5, 1.0);
        var v = u.value;
        if u.has_values != 0u {
            let vd = textureDimensions(values);
            v = textureLoad(values, min(t, vd - 1u), 0);
        }
        out.value = finite4(v);
    }
    return out;
}

@fragment
fn fs(v: Out) -> @location(0) vec4f {
    return v.value;
}
