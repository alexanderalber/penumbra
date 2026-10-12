// tex.denoise: a running average over the frames (in0 the new frame, in1 the node's last
// result) that lets go wherever the picture moves, plus an edge-keeping smoothing within the
// frame for what moves, where the average cannot help. Differences are judged on the square
// root of the values, near enough to the eye's response that `motion` means about the same in
// dark and bright parts. `history` (set by the node, not a user param) is 0 when there is no
// last result yet.

fn pn_perc(c: vec4f) -> vec4f {
    return sqrt(max(c, vec4f(0.0)));
}

fn pn_at(t: texture_2d<f32>, p: vec2i, size: vec2i) -> vec4f {
    return textureLoad(t, clamp(p, vec2i(0), size - 1), 0);
}

fn pn_unit(h: u32) -> f32 {
    return f32(h >> 8u) * (1.0 / 16777216.0);
}

// One step of a 16-bit float (the target's format) at x.
fn pn_step(x: f32) -> f32 {
    let e = max(floor(log2(max(abs(x), 1e-30))), -14.0);
    return exp2(e - 10.0);
}

// x rounded to a 16-bit float at random, up with the probability of how close it is: rounded
// to the nearest, a change smaller than half a step (1 / frames of the difference) is lost, and
// a long average stops short of the value it heads for. The result is exactly representable,
// so it does not depend on how the GPU rounds when it writes (WARP and an RTX 3090 both round
// toward zero: with a dither around the nearest, the average drifted down).
fn pn_round(x: f32, u: f32) -> f32 {
    let s = pn_step(x);
    return floor(x / s + u) * s;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let size = vec2i(textureDimensions(in0));
    let p = vec2i(v.pos.xy);
    let cur = pn_at(in0, p, size);
    let tol = max(params.motion, 1e-4);

    // Whether the pixel moved: the 3 x 3 mean of the new frame against the average so far, so
    // noise in single pixels is not taken for motion.
    var mean = vec4f(0.0);
    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            mean += pn_at(in0, p + vec2i(dx, dy), size);
        }
    }
    mean /= 9.0;
    let hist = pn_at(in1, p, size);
    let d = abs(pn_perc(mean) - pn_perc(hist));
    let diff = max(max(d.r, d.g), max(d.b, d.a));
    let history = clamp(params.history, 0.0, 1.0);
    let still = (1.0 - smoothstep(0.5 * tol, tol, diff)) * history;

    // Moving parts: a 5 x 5 bilateral filter, which leaves edges stronger than `motion` alone.
    var fresh = cur;
    let amount = clamp(params.spatial, 0.0, 1.0) * (1.0 - still);
    if (amount > 0.0) {
        let pc = pn_perc(cur);
        var sum = vec4f(0.0);
        var total = 0.0;
        for (var dy = -2; dy <= 2; dy++) {
            for (var dx = -2; dx <= 2; dx++) {
                let s = pn_at(in0, p + vec2i(dx, dy), size);
                let ds = pn_perc(s) - pc;
                let r2 = f32(dx * dx + dy * dy);
                let w = exp(-dot(ds, ds) / (2.0 * tol * tol) - r2 / 4.5);
                sum += w * s;
                total += w;
            }
        }
        fresh = mix(cur, sum / total, amount);
    }

    let keep = clamp(1.0 - 1.0 / max(params.frames, 1.0), 0.0, 1.0) * still;
    let out = mix(fresh, hist, keep);
    let h = pn_hash(vec3u(vec2u(p), u32(pn_time.frame)));
    return vec4f(
        pn_round(out.r, pn_unit(h.x)),
        pn_round(out.g, pn_unit(h.y)),
        pn_round(out.b, pn_unit(h.z)),
        // Alpha to the nearest: a picture opaque everywhere (a camera) would otherwise come
        // out a step below 1 here and there.
        pn_round(out.a, 0.5),
    );
}
