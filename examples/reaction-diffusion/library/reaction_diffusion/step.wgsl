// Gray-Scott steps. Each pass is one step and reads the pass before as in0; the node's
// `iterations` is the number of steps per frame.
//
// State texture: r = 1 - U, g = V, b = 0, a = 1. U rests at 1, so 1 - U rests at 0. In 32-bit
// floats this costs nothing; it also keeps the state usable when a GPU can only write 16-bit
// floats (they are much finer near 0 than near 1).
//
// in0: the state (from the tex.feedback; from the second pass on, the pass before).
// in1: the object mask, any size. While use_mask is on, the rates in use hold where it is
//      bright and the outside rates where it is dark; a soft edge mixes them.
// in2: people, any size. What a bright spot does is `touch`.
// in3: the seed (seed.wgsl). A revive by guard.wgsl starts the first pass from it.
// in4: guard.wgsl. Texel 1: r 1 to start over this frame, g 1 to stir this frame, b the number
//      of stirs so far (where the holes fall). Texel 2: the rates in use, feed, kill,
//      outside_feed, outside_kill (they glide there while morph is on).
//
// @param map bool = false -- feed rises from bottom to top and kill from left to right, along the band where patterns live, so every pattern shows side by side; feed and kill do not act while it is on
// @param use_mask bool = false -- the mask input picks the rates: feed and kill where it is bright, outside_feed and outside_kill where it is dark
// @param touch choice(off|wipe|seed|kill) = wipe -- what a bright spot of the people input does; wipe: clears the pattern, it grows back behind; seed: ignites new pattern; kill: the pattern dies away under it
// @param touch_strength float = 1 0..1 -- how strongly the people input acts
// @param pause bool = false -- holds the state
// @param edges choice(wrap|closed) = wrap -- wrap: what leaves one side comes in at the other; closed: a wall
// @param du float = 1.0 0.2..1.2 -- how fast U spreads
// @param dv float = 0.5 0.05..0.8 -- how fast V spreads; du / dv near 2 gives the classic patterns
// @param dt float = 1.0 0.1..1.2 -- time per step; above about 1.2 the simulation blows up
// @param stir_count int = 4 1..40 -- holes punched into a still pattern by one stir
// @param stir_size float = 40 4..120 -- radius of one hole, in texels

fn edge(q: vec2i, n: vec2i, closed: bool) -> vec2i {
    let wrapped = ((q % n) + n) % n;
    return select(wrapped, clamp(q, vec2i(0), n - vec2i(1)), closed);
}

fn rand2(a: u32, b: u32) -> vec2f {
    let h = pn_hash(vec3u(a, b, 4242u));
    return vec2f(f32(h.x >> 8u), f32(h.y >> 8u)) / 16777216.0;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let rates = textureLoad(in4, vec2i(2, 0), 0);
    let n = vec2i(textureDimensions(in0));
    let p = vec2i(v.pos.xy);
    let uv = (vec2f(p) + 0.5) / vec2f(n);
    var here = textureLoad(in0, p, 0);
    let flags = textureLoad(in4, vec2i(1, 0), 0);
    let first = pn.iteration == 0;
    let restart = first && flags.r > 0.5;
    if restart {
        here = textureSampleLevel(in3, pn_clamp, uv, 0.0);
    }
    if params.pause != 0u {
        return here;
    }
    let closed = params.edges == edges_closed;
    // w = 1 - U.
    var w = here.r;
    var vv = here.g;

    var fk = rates.xy;
    if params.map != 0u {
        // Pearson's map, sheared along the band where patterns live: F grows upwards, and k
        // grows to the right around the middle of that band at this F, so every pattern has
        // its place and little of the picture is flat.
        let f = mix(0.01, 0.075, 1.0 - uv.y);
        let kc = 0.044 + 0.55 * f - 4.0 * f * f;
        fk = vec2f(f, kc + (uv.x - 0.3) * 0.012);
    }
    if params.use_mask != 0u {
        let m = clamp(pn_luma(textureSampleLevel(in1, pn_clamp, uv, 0.0).rgb), 0.0, 1.0);
        fk = mix(rates.zw, fk, m);
    }
    let b = clamp(pn_luma(textureSampleLevel(in2, pn_clamp, uv, 0.0).rgb), 0.0, 1.0) * params.touch_strength;
    if params.touch == touch_kill {
        fk = mix(fk, vec2f(0.02, 0.09), b);
    }

    // Nine-point Laplacian (0.2 to the sides, 0.05 to the corners), written as weighted
    // differences so a flat field gives exactly 0. The state is read in its own texels; with
    // a restart in this pass the neighbours come from the seed too.
    var lap = vec2f(0.0);
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            if x != 0 || y != 0 {
                let q = edge(p + vec2i(x, y), n, closed);
                var t = textureLoad(in0, q, 0);
                if restart {
                    t = textureSampleLevel(in3, pn_clamp, (vec2f(q) + 0.5) / vec2f(n), 0.0);
                }
                let wt = select(0.05, 0.2, x == 0 || y == 0);
                lap += wt * (vec2f(t.r, t.g) - vec2f(w, vv));
            }
        }
    }
    let u = 1.0 - w;
    let uvv = u * vv * vv;
    // dU/dt = du lap U - U V^2 + F (1 - U); lap U = -lap w.
    let dw = -(-params.du * lap.x - uvv + fk.x * w);
    let dvv = params.dv * lap.y + uvv - (fk.x + fk.y) * vv;
    w = clamp(w + dw * params.dt, 0.0, 1.0);
    vv = clamp(vv + dvv * params.dt, 0.0, 1.0);

    if params.touch == touch_wipe {
        w = mix(w, 0.0, b);
        vv = mix(vv, 0.0, b);
    }
    if params.touch == touch_seed {
        w = mix(w, 0.5, b * 0.25);
        vv = mix(vv, 0.5, b * 0.25);
    }

    // A stir: a few holes back to rest, each with a spark of ignition in its middle.
    if first && flags.g > 0.5 {
        let r = params.stir_size;
        for (var i = 0; i < params.stir_count; i++) {
            let c = rand2(u32(flags.b), u32(i)) * vec2f(n);
            var d = abs(vec2f(p) - c);
            if !closed {
                d = min(d, vec2f(n) - d);
            }
            let l = length(d);
            if l < r {
                w = 0.0;
                vv = 0.0;
            }
            if l < max(r * 0.2, 3.0) {
                w = 0.5;
                vv = 0.5;
            }
        }
    }
    return vec4f(w, vv, 0.0, 1.0);
}
