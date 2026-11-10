// Walls for the ripples: white where waves cannot go. A drawn shape, the mask input (letters, a
// scanned object mask, a camera's silhouette), or both. Its size is the simulation's (in0).
//
// in0: the simulation's size (the component's world). in1: the mask, any size; where it is
// bright there is a wall while `use_mask` is on.
//
// @param shape choice(none|slit|double_slit|grating|pillars|box) = none -- a drawn wall: slit, double_slit, grating: a barrier across at `at` with gaps; pillars: a field of round posts; box: a frame inside the edges; none: no drawn wall
// @param use_mask bool = true -- the mask input stands in the water as walls; off: it is ignored
// @param at float = 0.35 0..1 -- where the barrier stands, from the left, in picture widths
// @param thick float = 6 1..40 -- barrier and frame thickness, in texels
// @param gap float = 14 2..200 -- width of each gap in the barrier, in texels
// @param apart float = 70 4..400 -- distance between gap centres, in texels
// @param count int = 7 1..30 -- gaps of the grating
// @param post float = 10 2..60 -- radius of the pillars, in texels

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let n = pn.resolution;
    let p = floor(v.pos.xy) + 0.5;
    var wall = false;
    let bx = n.x * params.at;
    let in_bar = abs(p.x - bx) <= params.thick * 0.5;
    let dy = p.y - n.y * 0.5;
    let count = f32(max(params.count, 1));
    if params.shape == shape_slit {
        wall = in_bar && abs(dy) > params.gap * 0.5;
    } else if params.shape == shape_double_slit {
        let a = abs(abs(dy) - params.apart * 0.5);
        wall = in_bar && a > params.gap * 0.5;
    } else if params.shape == shape_grating {
        // gap centres at (i - (count - 1) / 2) * apart, i = 0 .. count - 1
        let i = clamp(round(dy / params.apart + (count - 1.0) * 0.5), 0.0, count - 1.0);
        let c = (i - (count - 1.0) * 0.5) * params.apart;
        wall = in_bar && abs(dy - c) > params.gap * 0.5;
    } else if params.shape == shape_pillars {
        let cell = params.post * 6.0;
        let q = p - n * 0.5;
        let row = floor(q.y / cell + 0.5);
        let shift = select(0.0, 0.5 * cell, fract(row * 0.5) > 0.25);
        let g = vec2f(q.x - shift - cell * floor((q.x - shift) / cell + 0.5), q.y - cell * row);
        wall = length(g) <= params.post && abs(q.x) < n.x * 0.3;
    } else if params.shape == shape_box {
        let m = min(n.x, n.y) * 0.08;
        let e = min(min(p.x, n.x - p.x), min(p.y, n.y - p.y));
        wall = e > m && e < m + params.thick;
    }
    if params.use_mask != 0u {
        let c = textureSampleLevel(in1, pn_clamp, v.uv, 0.0);
        wall = wall || pn_luma(c.rgb) > 0.5;
    }
    let w = select(0.0, 1.0, wall);
    return vec4f(w, w, w, 1.0);
}
