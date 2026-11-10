// Numbers about the simulation, per block of cells, for a chan.from_tex with measure sum:
//   r: the share of all cells whose trail is above `mark` (the sum over all blocks is the
//      coverage, 0 to 1), g: the agents alive in the block (the sum is the number alive).
// in0: sets the size (a quarter of the simulation). in1: the trail. in2: the deposit.
//
// @param mark float = 0.3 0.01..0.9 -- a cell counts as covered where the trail shows at least this bright (with gain 0.04)

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = vec2i(v.pos.xy);
    let big = vec2i(textureDimensions(in1));
    let small = vec2i(textureDimensions(in0));
    let lo = p * big / small;
    let hi = (p + vec2i(1)) * big / small;
    // Trail amount at which 1 - exp(-0.04 s) reaches the mark.
    let level = -log(1.0 - params.mark) / 0.04;
    var covered = 0.0;
    var alive = 0.0;
    for (var y = lo.y; y < hi.y; y++) {
        for (var x = lo.x; x < hi.x; x++) {
            let t = textureLoad(in1, vec2i(x, y), 0);
            covered += select(0.0, 1.0, t.r + t.g + t.b > level);
            alive += textureLoad(in2, vec2i(x, y), 0).a;
        }
    }
    return vec4f(covered / f32(big.x * big.y), alive, 0.0, 1.0);
}
