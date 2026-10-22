// Flat 2D drawing (crates/gpu/src/canvas.rs): every primitive is one instanced quad over its
// bounds; the fragment shader finds the pixel's signed distance to the primitive's edge and
// covers it by that (one pixel of antialiasing). Colours are premultiplied and sRGB-encoded,
// blended in that space as an egui window does; the resolve pass turns the result into linear
// light at the end.

struct U {
    size: vec2f,
    atlas: vec2f,
}

struct Prim {
    // min x, min y, max x, max y of the quad in pixels
    bounds: vec4f,
    // rect, frame, glyph: min.xy, max.xy; arc: center.xy, radius, width; line: from.xy, to.xy
    a: vec4f,
    // rect, frame: radius, frame width; arc: fraction; line: width; glyph: atlas min.xy, max.xy
    b: vec4f,
    color: vec4f,
    // x: the kind
    kind: vec4u,
}

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var<storage, read> prims: array<Prim>;
@group(0) @binding(2) var atlas: texture_2d<f32>;
@group(0) @binding(3) var atlas_sampler: sampler;
@group(0) @binding(4) var scratch: texture_2d<f32>;

const RECT: u32 = 0u;
const FRAME: u32 = 1u;
const ARC: u32 = 2u;
const LINE: u32 = 3u;
const GLYPH: u32 = 4u;
const TAU: f32 = 6.283185307179586;

struct VOut {
    @builtin(position) pos: vec4f,
    @location(0) px: vec2f,
    @location(1) @interpolate(flat) index: u32,
}

@vertex
fn vs_draw(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VOut {
    var corners = array<vec2f, 6>(
        vec2f(0.0, 0.0), vec2f(1.0, 0.0), vec2f(0.0, 1.0),
        vec2f(0.0, 1.0), vec2f(1.0, 0.0), vec2f(1.0, 1.0),
    );
    let p = prims[ii];
    let xy = mix(p.bounds.xy, p.bounds.zw, corners[vi % 6u]);
    let ndc = xy / max(u.size, vec2f(1.0)) * 2.0 - 1.0;
    var out: VOut;
    out.pos = vec4f(ndc.x, -ndc.y, 0.0, 1.0);
    out.px = xy;
    out.index = ii;
    return out;
}

// Signed distance to a box of half size `half` centred at the origin with corner radius `r`.
fn rounded_box(p: vec2f, half: vec2f, r: f32) -> f32 {
    let rr = clamp(r, 0.0, min(half.x, half.y));
    let q = abs(p) - (half - vec2f(rr));
    return length(max(q, vec2f(0.0))) + min(max(q.x, q.y), 0.0) - rr;
}

fn cover(d: f32) -> f32 {
    return clamp(0.5 - d, 0.0, 1.0);
}

@fragment
fn fs_draw(v: VOut) -> @location(0) vec4f {
    let p = prims[v.index];
    let kind = p.kind.x;
    let q = v.px;
    var c = 0.0;
    if (kind == RECT || kind == FRAME) {
        let center = (p.a.xy + p.a.zw) * 0.5;
        let half = max((p.a.zw - p.a.xy) * 0.5, vec2f(0.0));
        let outer = rounded_box(q - center, half, p.b.x);
        if (kind == RECT) {
            c = cover(outer);
        } else {
            let w = p.b.y;
            let inner = rounded_box(q - center, max(half - vec2f(w), vec2f(0.0)), max(p.b.x - w, 0.0));
            c = cover(max(outer, -inner));
        }
    } else if (kind == ARC) {
        let d = q - p.a.xy;
        let r = length(d);
        let ring = abs(r - p.a.z) - p.a.w * 0.5;
        let fraction = clamp(p.b.x, 0.0, 1.0);
        var dist = ring;
        if (fraction < 1.0) {
            // Clockwise from the top (y points down).
            var angle = atan2(d.x, -d.y);
            if (angle < 0.0) {
                angle = angle + TAU;
            }
            let end = fraction * TAU;
            if (angle > end) {
                let off = min(angle - end, TAU - angle) * max(r, 1e-3);
                dist = max(ring, off);
            }
        }
        c = cover(dist);
    } else if (kind == LINE) {
        let a = p.a.xy;
        let b = p.a.zw;
        let len = length(b - a);
        let dir = select(vec2f(1.0, 0.0), (b - a) / len, len > 1e-6);
        let rel = q - a;
        let along = dot(rel, dir);
        let across = abs(dot(rel, vec2f(-dir.y, dir.x)));
        let dist = max(across - p.b.x * 0.5, max(-along, along - len));
        c = cover(dist);
    } else if (kind == GLYPH) {
        let size = max(p.a.zw - p.a.xy, vec2f(1e-6));
        let t = (q - p.a.xy) / size;
        let texel = mix(p.b.xy, p.b.zw, t);
        c = textureSampleLevel(atlas, atlas_sampler, texel / max(u.atlas, vec2f(1.0)), 0.0).r;
    }
    return p.color * c;
}

@vertex
fn vs_resolve(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    return pn_fullscreen(i);
}

@fragment
fn fs_resolve(@builtin(position) pos: vec4f) -> @location(0) vec4f {
    let c = textureLoad(scratch, vec2i(pos.xy), 0);
    let a = clamp(c.a, 0.0, 1.0);
    if (a <= 0.0) {
        return vec4f(0.0);
    }
    let straight = clamp(c.rgb / a, vec3f(0.0), vec3f(1.0));
    return vec4f(pn_srgb_decode(straight) * a, a);
}
