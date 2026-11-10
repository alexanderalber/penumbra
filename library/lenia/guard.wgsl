// The small state of the simulation, 5 x 1 texels in 32-bit floats, kept from frame to frame by a
// tex.feedback. It starts over with the component (key 0 or a param named on its @reset line).
//   texel 0: r 1 once the first frame has run, g seconds the world has been dead (coverage
//            below dead), b seconds it has been full (coverage above full), a the clock
//   texel 1: r 1 when step.wgsl starts over from the seed this frame, g 1 when it drops a
//            creature this frame, b 1 when it stirs, a the steps this frame
//   texel 2: r the part of a step carried over to the next frame, g seconds the world has been
//            still, b 0, a the restarts by the guard so far (seed.wgsl starts each differently)
//   texel 3: r restarts because the world died, g restarts because it filled up, b drops so far,
//            a stirs so far
//   texel 4: where the next drop lands: r, g as uv, b the angle
//
// in0: this node's own last frame (the tex.feedback).
// in1: measure.wgsl of the last frame (a tex.feedback): r coverage, g activity.
// in2: fields.wgsl: g the people (people_mode spawn drops creatures where they are), b above 0.5
//      where the mask lets nothing live.
// in3: the state of the last frame (the tex.feedback), so drops land on empty ground.
//
// @param people_mode choice(off|spawn|feed|kill|push) = spawn -- follows the component's people_mode
// @param pause bool = false -- follows the component's pause
// @param speed float = 1 0..4 -- follows the component's speed
// @param steps float = 2 0.1..16 -- follows the component's steps
// @param now float = 0 0..100000 -- seconds since the start; bound to the component's clock
// @param revive bool = true -- starts over once the coverage has stayed below dead for revive_after seconds
// @param revive_after float = 1 0..60 -- seconds a dead world stays before it starts over
// @param dead float = 0.0005 0..0.1 -- coverage below which the world counts as dead
// @param auto_restart bool = true -- starts over once the coverage has stayed above full for hold seconds
// @param full float = 0.08 0..1 -- coverage above which the world counts as full
// @param hold float = 1 0..60 -- seconds a full world stays before it starts over
// @param few float = 0.01 0..1 -- coverage below which creatures are dropped in at the refill rate
// @param refill float = 0.5 0..10 -- drops per second while the coverage is below few
// @param spawn float = 0.1 0..10 -- drops per second at any time, at random places
// @param rate float = 1 0..10 -- people_mode spawn: tries per second to drop a creature where someone is (a try fails where life already is)
// @param stir bool = false -- punches a few holes into a world that has stood still for stir_after seconds
// @param stir_after float = 15 1..600 -- seconds a still world stays before it is stirred
// @param still float = 0.3 0..10 -- activity below which the world counts as still

// Nothing alive at q nor at four places about a creature's width around it.
fn free(q: vec2f) -> bool {
    let n = vec2f(textureDimensions(in3));
    let d2 = vec2f(0.07 * n.y / n.x, 0.07);
    var most = 0.0;
    for (var i = 0; i < 5; i++) {
        var o = vec2f(0.0);
        if i == 1 { o = vec2f(d2.x, 0.0); } else if i == 2 { o = vec2f(-d2.x, 0.0); }
        else if i == 3 { o = vec2f(0.0, d2.y); } else if i == 4 { o = vec2f(0.0, -d2.y); }
        let a = textureSampleLevel(in3, pn_repeat, q + o, 0.0).rgb;
        most = max(most, max(max(a.r, a.g), a.b));
    }
    return most < 0.05;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let x = i32(v.pos.x);
    let s0 = textureLoad(in0, vec2i(0, 0), 0);
    let s2 = textureLoad(in0, vec2i(2, 0), 0);
    let s3 = textureLoad(in0, vec2i(3, 0), 0);
    let s4 = textureLoad(in0, vec2i(4, 0), 0);
    let m = textureLoad(in1, vec2i(0, 0), 0);
    let paused = params.pause != 0u;
    let started = s0.r > 0.5;
    let dt = clamp(params.now - s0.a, 0.0, 0.1);
    var restart = false;

    // Steps this frame: steps times speed, fractions carried over.
    var carry = s2.r;
    var todo = 0.0;
    if !paused {
        carry = carry + params.steps * params.speed;
        todo = min(floor(carry + 1e-4), 64.0);
        carry = clamp(carry - todo, 0.0, 1.0);
    }

    // The marks, on last frame's measure. Not in the first frame after a start.
    var dead_s = s0.g;
    var full_s = s0.b;
    var still_s = s2.g;
    let cov = m.r;
    let act = m.g;
    if !paused && started {
        dead_s = select(0.0, dead_s + dt, cov < params.dead);
        full_s = select(0.0, full_s + dt, cov > params.full);
        still_s = select(0.0, still_s + dt, act < params.still);
    }
    var deaths = s3.r;
    var fills = s3.g;
    var variant = s2.a;
    let live = !paused && started;
    if params.revive != 0u && live && cov < params.dead && dead_s >= params.revive_after {
        restart = true;
        deaths += 1.0;
        variant += 1.0;
    }
    if !restart && params.auto_restart != 0u && live && cov > params.full && full_s >= params.hold {
        restart = true;
        fills += 1.0;
        variant += 1.0;
    }

    // Drops: spawn at any time, refill while the world is too empty, and people spawn. The
    // random numbers come from the frame number; random is all they need to be.
    let h = pn_hash(vec3u(u32(pn_time.frame), 7u, 99u));
    let r4 = vec4f(f32(h.x >> 8u), f32(h.y >> 8u), f32(h.z >> 8u), f32((h.x ^ h.y) >> 8u)) / 16777216.0;
    var per_s = params.spawn;
    if cov < params.few {
        per_s = max(per_s, params.refill);
    }
    // A drop lands only where life may live and nothing lives yet, so it neither dies at once
    // nor merges with a creature (two gliders that meet often fill the field).
    var drop = false;
    var at = s4;
    let want = todo >= 1.0 && r4.w < per_s * dt;
    let under = todo >= 1.0 && params.people_mode == people_mode_spawn && r4.w < params.rate * dt;
    if want || under {
        for (var c = 0u; c < 64u; c++) {
            let hc = pn_hash(vec3u(u32(pn_time.frame), c, 123u));
            let q = vec2f(f32(hc.x >> 8u), f32(hc.y >> 8u)) / 16777216.0;
            let fq = textureSampleLevel(in2, pn_clamp, q, 0.0);
            var ok = !drop && fq.b < 0.5 && free(q);
            if !want {
                ok = ok && fq.g > 0.5;
            }
            if ok {
                drop = true;
                at = vec4f(q, r4.z * 6.2831853, 0.0);
            }
        }
    }
    var stir = false;
    if !restart && params.stir != 0u && still_s >= params.stir_after && todo >= 1.0 {
        stir = true;
        still_s = 0.0;
    }
    if restart {
        dead_s = 0.0;
        full_s = 0.0;
        still_s = 0.0;
        drop = false;
        stir = false;
    }
    let drops = s3.b + select(0.0, 1.0, drop);
    let stirs = s3.a + select(0.0, 1.0, stir);
    if x == 0 {
        return vec4f(1.0, dead_s, full_s, params.now);
    }
    if x == 1 {
        return vec4f(select(0.0, 1.0, restart), select(0.0, 1.0, drop), select(0.0, 1.0, stir), todo);
    }
    if x == 2 {
        return vec4f(carry, still_s, 0.0, variant);
    }
    if x == 3 {
        return vec4f(deaths, fills, drops, stirs);
    }
    return at;
}
