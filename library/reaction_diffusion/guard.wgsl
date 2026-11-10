// The small state of reaction-diffusion, 4 x 1 texels in 32-bit floats, kept from frame to
// frame by a tex.feedback. It starts over with the simulation.
//   texel 0: r 1 once it has run, g seconds the pattern has been dead, b seconds it has been
//            still, a the clock at the last frame
//   texel 1: r 1 when step.wgsl starts over from the seed this frame (a revive), g 1 when it
//            stirs this frame, b stirs so far, a revives so far
//   texel 2: the rates step.wgsl uses this frame: feed, kill, outside_feed, outside_kill
//   texel 3: the first stage of the glide toward the params (same order)
//
// The glide: while morph is on, the rates in use follow the params through two smoothing
// stages, so a new preset's rates arrive in an S-curve: 95 % of the way after `glide` seconds.
// A hand-dragged feed or kill glides the same way. With morph off they follow at once.
//
// in0: this node's own last frame (the tex.feedback).
//
// @param feed float = 0.0545 0.005..0.11 -- F, how fast U is fed in
// @param kill float = 0.062 0.045..0.1 -- k, how fast V is removed on top of F
// @param outside_feed float = 0.02 0.005..0.11 -- F where the mask is dark, while use_mask is on
// @param outside_kill float = 0.09 0.045..0.1 -- k where the mask is dark, while use_mask is on
// @param morph bool = false -- feed and kill glide to new values instead of jumping
// @param glide float = 6 0..60 -- with morph: seconds in which feed and kill get 95 % of the way to new values; 0: at once
// @param now float = 0 0..100000 -- seconds since the start; bound to the component's clock
// @param activity float = 1 0..100 -- how much the picture changes (the component's activity output, a frame late)
// @param coverage float = 1 0..1 -- share of the counted area with pattern (the coverage output, a frame late)
// @param revive bool = true -- starts over from the seed once the pattern has been dead for revive_after seconds
// @param revive_after float = 3 0.1..120 -- seconds a dead pattern stays before it starts over
// @param dead float = 0.002 0..0.5 -- coverage below which (or above 1 minus which) the pattern counts as dead
// @param stir bool = true -- punches a few holes into a pattern that has been still for stir_after seconds
// @param stir_after float = 20 0.1..600 -- seconds a still pattern stays before it is stirred
// @param still float = 0.006 0..1 -- activity below which the pattern counts as still
// @param pause bool = false -- holds the state

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let x = i32(v.pos.x);
    let s0 = textureLoad(in0, vec2i(0, 0), 0);
    let s1 = textureLoad(in0, vec2i(1, 0), 0);
    let s2 = textureLoad(in0, vec2i(2, 0), 0);
    let s3 = textureLoad(in0, vec2i(3, 0), 0);
    let fresh = s0.r < 0.5;
    let paused = params.pause != 0u;
    var dt = clamp(params.now - s0.a, 0.0, 0.1);
    if fresh || paused {
        dt = 0.0;
    }

    var dead_s = s0.g;
    var still_s = s0.b;
    if !paused {
        // Dead: nothing left, or one flat colour everywhere (a flood, after some morphs).
        dead_s = select(0.0, s0.g + dt, params.coverage < params.dead || params.coverage > 1.0 - params.dead);
        still_s = select(0.0, s0.b + dt, params.activity < params.still);
    }
    var revives = s1.a;
    var restart = false;
    if params.revive != 0u && dead_s >= params.revive_after {
        restart = true;
        revives += 1.0;
    }
    var stirring = false;
    if !restart && params.stir != 0u && still_s >= params.stir_after {
        stirring = true;
        still_s = 0.0;
    }
    if restart {
        dead_s = 0.0;
        still_s = 0.0;
    }
    let stirs = s1.b + select(0.0, 1.0, stirring);

    // The glide: two stages, each moving a share a of its distance per frame.
    let goal = vec4f(params.feed, params.kill, params.outside_feed, params.outside_kill);
    var first = goal;
    var rates = goal;
    if !fresh && !restart && params.morph != 0u && params.glide > 0.0 {
        let a = 1.0 - exp(-dt * 4.74 / params.glide);
        first = mix(s3, goal, a);
        rates = mix(s2, first, a);
    }

    if x == 0 {
        return vec4f(1.0, dead_s, still_s, params.now);
    }
    if x == 1 {
        return vec4f(select(0.0, 1.0, restart), select(0.0, 1.0, stirring), stirs, revives);
    }
    if x == 2 {
        return rates;
    }
    return first;
}
