// The start of a Gray-Scott simulation: U = 1, V = 0 everywhere (nothing happens there), and
// U = 0.5, V = 0.5 in the seeded spots (there the reaction ignites).
// Written as the state texture of step.wgsl: r = 1 - U, g = V, b = 0, a = 1.
// Its size is in0's (the component's world); the pattern's features are about 10 to 20
// texels wide whatever the size.
//
// in0: the world (only its size counts). in1: the object mask, any size; pattern `mask`
// seeds only where it is bright.
//
// @param pattern choice(blobs|square|noise|rings|mask|empty) = blobs -- where the reaction starts; blobs: round blobs at random places; square: one square in the middle; noise: small specks; rings: rings around the middle; mask: blobs only where the mask is bright; empty: nothing
// @param seed int = 1 0..1000 -- another seed gives other random blobs and noise
// @param size float = 8 1..40 -- blob radius, half the square's width, ring width, in texels
// @param density float = 0.35 0..1 -- blobs: share of places with a blob; noise: share of seeded specks (x 0.05)

fn rand(p: vec2u, k: u32) -> f32 {
    let h = pn_hash(vec3u(p, k + u32(params.seed) * 7919u));
    return f32(h.x >> 8u) / 16777216.0;
}

fn rand2(p: vec2u, k: u32) -> vec2f {
    let h = pn_hash(vec3u(p, k + u32(params.seed) * 7919u));
    return vec2f(f32(h.x >> 8u), f32(h.y >> 8u)) / 16777216.0;
}

// Written with one exit and no switch: on Direct3D 12 the FXC compiler rejected the
// version with returns inside switch cases and loops.
fn seeded(p: vec2f, n: vec2f, uv: vec2f) -> bool {
    let pat = params.pattern;
    let s = max(params.size, 1.0);
    let dens = params.density;
    var hit = false;
    if pat == pattern_square {
        let d = abs(p - floor(n * 0.5));
        hit = max(d.x, d.y) <= s;
    } else if pat == pattern_noise {
        // 3 x 3 texel specks: a single texel dies before it ignites in most presets.
        hit = rand(vec2u(floor(p / 3.0)), 3u) < dens * 0.05;
    } else if pat == pattern_rings {
        let d = length(p - n * 0.5);
        hit = d < min(n.x, n.y) * 0.46 && fract(floor(d / s) * 0.25) < 0.1;
    } else if pat == pattern_blobs || pat == pattern_mask {
        // One candidate per cell of 6 sizes (3 for mask), kept with probability density;
        // mask keeps only the blobs where in1 is bright.
        let cell = select(6.0, 3.0, pat == pattern_mask) * s;
        let c = floor(p / cell);
        for (var y = -1; y <= 1; y++) {
            for (var x = -1; x <= 1; x++) {
                let q = c + vec2f(f32(x), f32(y));
                let kk = vec2u(vec2i(q) + vec2i(10000));
                let centre = (q + rand2(kk, 13u)) * cell;
                let r = s * (0.6 + rand(kk, 17u));
                hit = hit || (rand(kk, 11u) < dens && length(p - centre) <= r);
            }
        }
    }
    if pat == pattern_mask {
        hit = hit && pn_luma(textureSampleLevel(in1, pn_clamp, uv, 0.0).rgb) > 0.5;
    }
    return hit;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let n = pn.resolution;
    let p = floor(v.pos.xy);
    if seeded(p, n, v.uv) {
        return vec4f(0.5, 0.5, 0.0, 1.0);
    }
    return vec4f(0.0, 0.0, 0.0, 1.0);
}
