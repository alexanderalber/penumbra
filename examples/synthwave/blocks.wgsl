// The city: one block per texel, drawn by geo.tex_instance. A column of texels is a street
// across (x), a row a place along it (y). Every block comes towards the camera at `speed` and,
// once past it, starts again far ahead with a new size and place. Low blocks stay in the lane
// down the middle, so the eye can fly over them.
// @param output choice(position|scale) = position -- what this node hands on: where each block stands, or its size
// @param speed float = 1.0417 0..4 -- metres per second towards the camera; the grid moves as fast
// @param near float = -1 -10..10 -- where the blocks leave, behind the camera
// @param far float = 11 1..50 -- where they appear, growing out of the floor
// @param spread float = 12 1..40 -- width of the city across
// @param lane float = 1 0..5 -- half the width of the low lane in the middle
// @param fill float = 0.7 0..1 -- share of places that hold a block
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let cell = vec2u(v.pos.xy);
    let size = pn.resolution;
    let span = params.far - params.near;
    let spacing = span / size.y;
    // Each street runs a little out of step with its neighbours.
    let street = f32(pn_hash(vec3u(cell.x, 7u, 0u)).x) / 4294967296.0;
    let travelled = pn_time.seconds * params.speed + (f32(cell.y) + street) * spacing;
    let lap = floor(travelled / span);
    let y = params.far - (travelled - lap * span);
    // A new block on every lap: three random numbers for its side, place and height.
    let r = vec3f(pn_hash(vec3u(cell, u32(lap)))) / 4294967296.0;
    let slot = params.spread / size.x;
    let side = min(mix(0.25, 0.5, r.x), 0.9 * min(slot, spacing));
    let x = -0.5 * params.spread + (f32(cell.x) + 0.5) * slot + (r.y - 0.5) * (slot - side);
    var height = select(mix(0.2, 1.4, r.z), mix(0.04, 0.24, r.z), abs(x) < params.lane);
    // Grow out of the floor over the first two metres; leave out the empty places.
    height *= smoothstep(params.far, params.far - 2.0, y);
    let present = fract(r.x * 97.0 + r.z * 13.0) < params.fill;
    if !present {
        height = 0.0;
    }
    if params.output == output_scale {
        return vec4f(select(0.0, side, present), select(0.0, side, present), height, 1.0);
    }
    return vec4f(x, y, 0.5 * height, 1.0);
}
