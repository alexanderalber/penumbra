// tex.liquid's obstacles: a mask picture to the solid field the simulation reads (`solid`, one
// vec4f per grid node: signed distance in picture heights, negative inside; the solid's velocity
// in heights per second; its friction factor).
//
// Jump flooding on a lattice of square mask cells half a grid cell wide, over the picture. Cell
// (i, j) has its centre at ((i, j) + 0.5) * half a cell, in heights from the top left.
//
//   seed     pic0 the mask: each cell knows itself as solid (brighter than half) or empty
//   flood    one jump of `params.step` cells, from one seed buffer into the other
//            (`params.from_b`): each cell keeps the nearest solid and the nearest empty cell
//            its neighbours know
//   resolve  pic1 the obstacles' velocity: per grid node the distance to the nearest solid
//            surface, joined with the picture's edges when they are walls
//
// The steps run from half the larger side down to 1, then 1 once more.

const PAD: f32 = 3.0;
const FAR: f32 = 1000.0;

struct J {
    size: vec4u, // mask cells across, down; grid nodes across, down
    room: vec4f, // the picture's width and height in heights; a mask cell's side; a grid cell's side
    edge: vec4f, // 1 for open edges; the fastest obstacle, heights per second; unused, unused
}

@group(2) @binding(0) var<uniform> j: J;
// Per mask cell: the nearest solid cell (xy) and the nearest empty cell (zw), -1 for none yet.
@group(2) @binding(1) var<storage, read_write> sa: array<vec4i>;
@group(2) @binding(2) var<storage, read_write> sb: array<vec4i>;
@group(2) @binding(3) var<storage, read_write> solid: array<vec4f>;

fn mask_size() -> vec2i {
    return vec2i(j.size.xy);
}

fn mask_index(c: vec2i) -> u32 {
    return u32(c.y) * j.size.x + u32(c.x);
}

// Where mask cell `c` lies on the picture, from 0 to 1.
fn mask_uv(c: vec2f) -> vec2f {
    return (c + vec2f(0.5)) * j.room.z / j.room.xy;
}

@compute @workgroup_size(16, 4)
fn seed(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= pn.size.xy)) {
        return;
    }
    let c = vec2i(id.xy);
    let m = textureSampleLevel(pic0, pn_clamp, mask_uv(vec2f(c)), 0.0);
    // NaN fails the test: a broken mask is empty there.
    if (pn_luma(m.rgb) > 0.5) {
        sa[mask_index(c)] = vec4i(c, -1, -1);
    } else {
        sa[mask_index(c)] = vec4i(-1, -1, c);
    }
}

fn dist2(a: vec2i, b: vec2i) -> f32 {
    let d = vec2f(a - b);
    return dot(d, d);
}

fn seeds(i: u32, from_b: bool) -> vec4i {
    if (from_b) {
        return sb[i];
    }
    return sa[i];
}

@compute @workgroup_size(16, 4)
fn flood(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= pn.size.xy)) {
        return;
    }
    let c = vec2i(id.xy);
    let step = i32(params.step);
    let from_b = params.from_b > 0.5;
    let size = mask_size();
    var best = vec4i(-1);
    var ds = 1e30;
    var de = 1e30;
    for (var oy = -1; oy <= 1; oy++) {
        for (var ox = -1; ox <= 1; ox++) {
            let q = c + vec2i(ox, oy) * step;
            if (any(q < vec2i(0)) || any(q >= size)) {
                continue;
            }
            let s = seeds(mask_index(q), from_b);
            if (s.x >= 0) {
                let d = dist2(s.xy, c);
                if (d < ds) {
                    ds = d;
                    best = vec4i(s.xy, best.zw);
                }
            }
            if (s.z >= 0) {
                let d = dist2(s.zw, c);
                if (d < de) {
                    de = d;
                    best = vec4i(best.xy, s.zw);
                }
            }
        }
    }
    let i = mask_index(c);
    if (from_b) {
        sa[i] = best;
    } else {
        sb[i] = best;
    }
}

// The obstacles' velocity at mask cell `c`, in heights per second: pic1's red in widths, green
// in heights, per second. Nothing wired reads zero.
fn velocity_at(c: vec2i) -> vec2f {
    let v = textureSampleLevel(pic1, pn_clamp, mask_uv(vec2f(c)), 0.0).rg * vec2f(j.room.x, 1.0);
    if (!(all(v == v) && all(abs(v) < vec2f(1e30)))) {
        return vec2f(0.0);
    }
    let len = length(v);
    if (len > j.edge.y) {
        return v * (j.edge.y / len);
    }
    return v;
}

@compute @workgroup_size(16, 4)
fn resolve(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= pn.size.xy)) {
        return;
    }
    let h = j.room.z;
    // The node in heights, then in mask cells (cell centres at whole numbers).
    let x = (vec2f(id.xy) - vec2f(PAD)) * j.room.w;
    let m = x / h - vec2f(0.5);
    let c0 = vec2i(floor(m));
    let size = mask_size();
    var ds = 1e30;
    var de = 1e30;
    var ps = vec2i(-1);
    for (var oy = 0; oy <= 1; oy++) {
        for (var ox = 0; ox <= 1; ox++) {
            let q = clamp(c0 + vec2i(ox, oy), vec2i(0), size - vec2i(1));
            let s = seeds(mask_index(q), params.from_b > 0.5);
            if (s.x >= 0) {
                let d = distance(vec2f(s.xy), m);
                if (d < ds) {
                    ds = d;
                    ps = s.xy;
                }
            }
            if (s.z >= 0) {
                de = min(de, distance(vec2f(s.zw), m));
            }
        }
    }
    // The surface lies half a mask cell beyond the centre of the nearest cell across it.
    var o = FAR;
    if (ds < de) {
        o = -min((de - 0.5) * h, FAR);
    } else if (ds < 1e29) {
        o = min((ds - 0.5) * h, FAR);
    }
    var d = o;
    var v = vec2f(0.0);
    if (ps.x >= 0) {
        v = velocity_at(ps);
    }
    if (j.edge.x < 0.5) {
        let e = min(min(x.x, j.room.x - x.x), min(x.y, j.room.y - x.y));
        if (e < o) {
            d = e;
            v = vec2f(0.0);
        }
    }
    solid[u32(id.y) * j.size.z + u32(id.x)] = vec4f(d, v, 1.0);
}
