// The floor grid of the synthwave stage as rows of cells, one row per texel, drawn by
// geo.tex_instance with strip.obj (a row of 80 cells, 40 m wide and one cell deep). Row k
// stands at y = -20 + 0.5 k; the stage moves all rows together by its scroll, so the rows
// together are the grid of 80 by 80 cells it always was. A row in a gap of the floor is left
// out, and one in a gap that is opening fades.
// in0: the stage's lane picture (lane.wgsl): alpha is the floor, 1 where it stands, 0 in a gap.
// Out, by `what`: place, the row's corner (x, y, z); size, its scale (0 where the row is gone);
// light, its colour (the share of the floor left). Render it 80 x 1.
// @param what choice(place|size|light) = place -- which picture
// @param scroll float = 0 -1..1 -- how far the grid is moved along y now, in metres (the stage's scroll)

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;
const ROWS: u32 = 80u;
const CELL: f32 = 0.5;

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let cell = vec2u(v.pos.xy);
    let k = cell.x + cell.y * u32(pn.resolution.x);
    if k >= ROWS {
        return vec4f(0.0);
    }
    let y = -20.0 + CELL * f32(k);
    if params.what == what_place {
        return vec4f(0.0, y, 0.0, 1.0);
    }
    // The floor at the row's middle: a gap's edges fall on cell edges, so the middle is wholly
    // in or out of it.
    let middle = y + 0.5 * CELL + params.scroll;
    let u = (middle - LANE_NEAR) / (LANE_FAR - LANE_NEAR);
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let wired = lane0.b > 0.0;
    let ground = select(1.0, textureSampleLevel(in0, pn_clamp, vec2f(u, 0.5), 0.0).a, wired);
    let left = clamp(ground, 0.0, 1.0);
    if params.what == what_size {
        let s = select(1.0, 0.0, left < 0.02);
        return vec4f(s, s, s, 1.0);
    }
    return vec4f(left, left, left, 1.0);
}
