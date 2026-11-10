// Lenia steps: continuous cellular automata ("artificial life"). Each pass is one step and
// reads the pass before as in0. The node's `iterations` is the most steps a frame may take;
// guard.wgsl says how many this frame really takes, and the passes beyond that only hand the
// state on.
//
// One step: every texel looks at a ring-shaped neighbourhood of radius R (the kernel), takes the
// weighted mean U of the state there, and grows where U is near `mu` (within about `sigma`) and
// shrinks everywhere else: A += dt * (2 exp(-(U - mu)^2 / (2 sigma^2)) - 1), clipped to 0..1.
//
// R is `radius` times `scale`, measured in texels of a simulation 288 texels high, so a creature
// covers the same share of the picture at any simulation size.
//
// State texture: r, g, b = up to three living channels (rules lenia and smoothlife use r only),
// a = 1.
//
// in0: the state (from the tex.feedback; from the second pass on, the pass before).
// in1: fields.wgsl at the simulation size: r the mask, g the people, b how fast life dies here
//      (the mask), a 1 where the rules hold as set and 0 where the outside rules do.
// in2: guard.wgsl. Texel 1: r 1 to start over from in4 this frame, g 1 to drop a creature (at
//      texel 4: x, y as uv, b the angle), b 1 to stir (punch holes), a the steps this frame.
//      Texel 3: b drops so far, a stirs so far (where the holes fall).
// in3: the creatures: square slots side by side (glider, ring), each for a radius of 13 texels.
// in4: the seed (seed.wgsl).
//
// @param rule choice(lenia|smoothlife|three) = lenia -- follows the component's rule
// @param radius float = 17.33 4..40 -- follows the component's radius
// @param scale float = 1 0.25..4 -- follows the component's scale
// @param creature choice(none|glider|ring) = glider -- follows the component's creature
// @param people_mode choice(off|spawn|feed|kill|push) = spawn -- follows the component's people_mode
// @param pause bool = false -- holds the state
// @param rings vec3 = [1, 0, 0] -- heights of up to three rings of the kernel, centre outwards; the last one above 0 ends the list
// @param core choice(bump|soft|flat) = bump -- the shape of one ring; bump: smooth Lenia ring, soft: wider, flat: hard-edged
// @param mu float = 0.15 0.05..0.5 -- growth centre: the neighbourhood mean where life grows
// @param sigma float = 0.0145 0.003..0.1 -- growth width; too narrow dies, too wide fills the field
// @param dt float = 0.1 0.01..1 -- time per step; smaller is smoother and slower
// @param outside_size float = 0.6 0.2..2 -- mask_mode rules: the kernel where the mask is dark, as a share of the radius; below 1 gives finer creatures there
// @param edges choice(wrap|closed) = wrap -- wrap: what leaves one side comes in at the other; closed: nothing beyond the edges
// @param push float = 1 0..4 -- people_mode push: how far life is pushed away from people per step
// @param stir_count int = 4 1..20 -- holes punched into a still world by one stir

// One rule set: up to three kernel shapes and up to six growth terms.
// Shape s: radius factor r[s] (0 = unused, 1 = R), ring heights b0/b1/b2, core.
// Growth term t: g[t] = (mu, sigma, weight h, kind); gi[t] = (shape, source channel, target channel, on).
// kind 0: Lenia growth h * (2 exp(-(U - mu)^2 / (2 sigma^2)) - 1);
// kind 3: cyclic, h * ((1 + a) U_(src+1) + (1 - a) U_(src+2) - mu), a in the sigma slot.
struct Rule {
    r: vec3f,
    b0: vec3f,
    b1: vec3f,
    b2: vec3f,
    core: vec3i,
    g0: vec4f, g1: vec4f, g2: vec4f, g3: vec4f, g4: vec4f, g5: vec4f,
    i0: vec4i, i1: vec4i, i2: vec4i, i3: vec4i, i4: vec4i, i5: vec4i,
    dt: f32,
    smooth_life: i32,
}

const OFF = vec4i(0, 0, 0, 0);
const NOG = vec4f(0.15, 0.015, 0.0, 0.0);

// rule lenia: one Lenia rule on channel r. rule smoothlife: SmoothLife, a disk of 0.256 R and a
// ring out to 0.77 R. rule three: three channels, each with the same Lenia rule on itself, each
// held back by the other two, more by the next channel than by the one after
// (rock-paper-scissors).
fn rule() -> Rule {
    let core = params.core;
    var out = Rule(vec3f(1.0, 0.0, 0.0), params.rings, vec3f(0.0), vec3f(0.0), vec3i(core, 0, 0),
        vec4f(params.mu, params.sigma, 1.0, 0.0), NOG, NOG, NOG, NOG, NOG,
        vec4i(0, 0, 0, 1), OFF, OFF, OFF, OFF, OFF, params.dt, 0);
    if params.rule == rule_smoothlife {
        out = Rule(vec3f(0.256, 0.77, 0.0), vec3f(1.0, 0.0, 0.0), vec3f(0.0, 1.0, 1.0), vec3f(0.0),
            vec3i(core_flat, core_flat, 0), NOG, NOG, NOG, NOG, NOG, NOG, OFF, OFF, OFF, OFF, OFF, OFF, params.dt, 1);
    } else if params.rule == rule_three {
        let g = vec4f(params.mu, params.sigma, 1.0, 0.0);
        let c = vec4f(0.0, 0.6, -2.0, 3.0);
        out = Rule(vec3f(1.0, 0.0, 0.0), params.rings, vec3f(0.0), vec3f(0.0), vec3i(core, 0, 0),
            g, g, g, c, c, c,
            vec4i(0, 0, 0, 1), vec4i(0, 1, 1, 1), vec4i(0, 2, 2, 1), vec4i(0, 0, 0, 1), vec4i(0, 1, 1, 1), vec4i(0, 2, 2, 1),
            params.dt, 0);
    }
    return out;
}

// Weight of a kernel shape at q = distance / its radius (0..1).
fn shell(q: f32, b: vec3f, core: i32) -> f32 {
    var nb = 1.0;
    if b.z > 0.0 { nb = 3.0; } else if b.y > 0.0 { nb = 2.0; }
    let x = q * nb;
    let f = fract(x);
    var h = b.x;
    if x >= 2.0 { h = b.z; } else if x >= 1.0 { h = b.y; }
    var w = 0.0;
    if core == core_flat {
        w = 1.0;
    } else if core == core_soft {
        let s = 4.0 * f * (1.0 - f);
        w = s * s * s * s;
    } else if f > 0.0 && f < 1.0 {
        w = exp(4.0 - 1.0 / (f * (1.0 - f)));
    }
    var out = 0.0;
    if q < 1.0 { out = h * w; }
    return out;
}

fn shell4(q: vec4f, b: vec3f, core: i32) -> vec4f {
    return vec4f(shell(q.x, b, core), shell(q.y, b, core), shell(q.z, b, core), shell(q.w, b, core));
}

fn growth(u: f32, mu: f32, sigma: f32) -> f32 {
    let d = (u - mu) / sigma;
    return 2.0 * exp(-0.5 * d * d) - 1.0;
}

fn pick(v: vec4f, i: i32) -> f32 {
    var x = v.x;
    if i == 1 { x = v.y; } else if i == 2 { x = v.z; }
    return x;
}

fn put(i: i32, x: f32) -> vec3f {
    var v = vec3f(x, 0.0, 0.0);
    if i == 1 { v = vec3f(0.0, x, 0.0); } else if i == 2 { v = vec3f(0.0, 0.0, x); }
    return v;
}

// SmoothLife transition (Rafler): new state from the disk mean m and the ring mean n.
fn sigm(x: f32, a: f32, w: f32) -> f32 {
    return 1.0 / (1.0 + exp(-(x - a) * 4.0 / w));
}

fn smooth_life(n: f32, m: f32) -> f32 {
    let b1 = 0.278; let b2 = 0.365; let d1 = 0.267; let d2 = 0.445;
    let an = 0.028; let am = 0.147;
    let alive = sigm(m, 0.5, am);
    let lo = mix(b1, d1, alive);
    let hi = mix(b2, d2, alive);
    return sigm(n, lo, an) * (1.0 - sigm(n, hi, an));
}

fn edge(q: vec2i, n: vec2i, closed: bool) -> vec2i {
    let wrapped = ((q % n) + n) % n;
    return select(wrapped, clamp(q, vec2i(0), n - vec2i(1)), closed);
}

fn term(g: vec4f, i: vec4i, u0: vec4f, u1: vec4f, u2: vec4f) -> vec3f {
    var u = u0;
    if i.x == 1 { u = u1; } else if i.x == 2 { u = u2; }
    var d = vec3f(0.0);
    if i.w != 0 {
        let x = pick(u, i.y);
        var gr = growth(x, g.x, g.y);
        if g.w > 2.5 {
            let c1 = pick(u, (i.y + 1) % 3);
            let c2 = pick(u, (i.y + 2) % 3);
            gr = c1 * (1.0 + g.y) + c2 * (1.0 - g.y) - g.x;
        }
        d = put(i.z, g.z * gr);
    }
    return d;
}

fn rand2(a: u32, b: u32) -> vec2f {
    let h = pn_hash(vec3u(a, b, 4243u));
    return vec2f(f32(h.x >> 8u), f32(h.y >> 8u)) / 16777216.0;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let n = vec2i(textureDimensions(in0));
    let p = vec2i(v.pos.xy);
    let nf = vec2f(n);
    let uv = (vec2f(p) + 0.5) / nf;
    let flags = textureLoad(in2, vec2i(1, 0), 0);
    let first = pn.iteration == 0;
    if first && flags.r > 0.5 {
        // A restart: the seed is the state from here on (this frame's first step is skipped).
        return textureLoad(in4, min(p, vec2i(textureDimensions(in4)) - vec2i(1)), 0);
    }
    var here = textureLoad(in0, p, 0);
    if params.pause != 0u || f32(pn.iteration) >= flags.a {
        return here;
    }
    let people = params.people_mode;
    let multi = params.rule == rule_three;
    let k = rule();
    let closed = params.edges == edges_closed;
    let radius = params.radius * params.scale * nf.y / 288.0;
    let f = textureLoad(in1, p, 0);

    // A drop: a creature (or, with creature none, a disc of random cells) at the place
    // guard.wgsl chose. The drop skips this step: a growth step on a neighbourhood that did not
    // contain it yet would take dt off every cell and kill the newcomer.
    if first && flags.g > 0.5 {
        let d4 = textureLoad(in2, vec2i(4, 0), 0);
        let centre = d4.xy * nf;
        var dd = vec2f(p) + 0.5 - centre;
        dd = dd - nf * round(dd / nf);
        if params.creature != creature_none {
            let dim = vec2f(textureDimensions(in3));
            let slots = max(floor(dim.x / dim.y), 1.0);
            let slot = f32(params.creature - 1);
            let sz = 48.0 * radius / 13.0;
            let ang = d4.z;
            let local = vec2f(cos(ang) * dd.x + sin(ang) * dd.y, -sin(ang) * dd.x + cos(ang) * dd.y);
            let suv = local / sz + 0.5;
            if all(suv > vec2f(0.0)) && all(suv < vec2f(1.0)) {
                let au = vec2f((slot + suv.x) / slots, suv.y);
                return vec4f(max(here.rgb, textureSampleLevel(in3, pn_clamp, au, 0.0).rgb), 1.0);
            }
        } else if length(dd) < radius * 0.9 {
            let hc = pn_hash(vec3u(vec2u(p), u32(textureLoad(in2, vec2i(3, 0), 0).b) + 77u));
            let cells = vec3f(f32(hc.x >> 8u), f32(hc.y >> 8u), f32(hc.z >> 8u)) / 16777216.0;
            var ch = vec3f(1.0, 0.0, 0.0);
            if multi {
                let c = floor(fract(d4.z / 6.2831853) * 3.0);
                ch = vec3f(select(0.0, 1.0, c == 0.0), select(0.0, 1.0, c == 1.0), select(0.0, 1.0, c >= 2.0));
            }
            return vec4f(cells * ch, 1.0);
        }
    }

    // Rules outside the mask: the kernel scaled (other-sized creatures).
    let inside = f.a;
    let scale = mix(params.outside_size, 1.0, inside);
    let big = radius * max(params.outside_size, 1.0);
    let rr = k.r * radius * scale;
    let ext = i32(ceil(max(max(k.r.x, k.r.y), k.r.z) * big));

    // The kernel sum, 2 x 2 texels per textureGather (four times fewer fetches than
    // textureLoad, and the repeat sampler does the wrapping). Channels g and b are read only
    // by rule three.
    var u0 = vec4f(0.0); var u1 = vec4f(0.0); var u2 = vec4f(0.0);
    var s = vec3f(0.0);
    let lim = f32(ext) + 1.5;
    for (var y = -ext; y <= ext; y += 2) {
        for (var x = -ext; x <= ext; x += 2) {
            if length(vec2f(f32(x) + 0.5, f32(y) + 0.5)) < lim {
                let c = (vec2f(p + vec2i(x, y)) + 1.0) / nf;
                let gr = textureGather(0, in0, pn_repeat, c);
                var gg = vec4f(0.0);
                var gb = vec4f(0.0);
                if multi {
                    gg = textureGather(1, in0, pn_repeat, c);
                    gb = textureGather(2, in0, pn_repeat, c);
                }
                // Texel offsets of the gathered components: x (0, 1), y (1, 1), z (1, 0), w (0, 0).
                let ox = vec4f(0.0, 1.0, 1.0, 0.0) + f32(x);
                let oy = vec4f(1.0, 1.0, 0.0, 0.0) + f32(y);
                let d = sqrt(ox * ox + oy * oy);
                var ins = vec4f(1.0);
                if closed {
                    let tx = vec4f(f32(p.x)) + ox;
                    let ty = vec4f(f32(p.y)) + oy;
                    ins = step(vec4f(0.0), tx) * step(tx, vec4f(nf.x - 1.0)) * step(vec4f(0.0), ty) * step(ty, vec4f(nf.y - 1.0));
                }
                let w0 = shell4(d / rr.x, k.b0, k.core.x) * ins;
                u0 += vec4f(dot(w0, gr), dot(w0, gg), dot(w0, gb), 0.0);
                s.x += dot(w0, vec4f(1.0));
                if k.r.y > 0.0 {
                    let w1 = shell4(d / rr.y, k.b1, k.core.y) * ins;
                    u1 += vec4f(dot(w1, gr), dot(w1, gg), dot(w1, gb), 0.0);
                    s.y += dot(w1, vec4f(1.0));
                }
                if k.r.z > 0.0 {
                    let w2 = shell4(d / rr.z, k.b2, k.core.z) * ins;
                    u2 += vec4f(dot(w2, gr), dot(w2, gg), dot(w2, gb), 0.0);
                    s.z += dot(w2, vec4f(1.0));
                }
            }
        }
    }
    u0 /= max(s.x, 1e-6); u1 /= max(s.y, 1e-6); u2 /= max(s.z, 1e-6);

    // people push: what is here comes from a little further away from the person, so life
    // drifts out of their way (the kernel sum above stays unmoved; the shift is under a texel).
    var a = here.rgb;
    if people == people_mode_push {
        let gx = textureLoad(in1, edge(p + vec2i(2, 0), n, true), 0).g - textureLoad(in1, edge(p - vec2i(2, 0), n, true), 0).g;
        let gy = textureLoad(in1, edge(p + vec2i(0, 2), n, true), 0).g - textureLoad(in1, edge(p - vec2i(0, 2), n, true), 0).g;
        var sh = vec2f(gx, gy) * params.push * 2.0;
        let l = length(sh);
        if l > 1.0 { sh = sh / l; }
        a = textureSampleLevel(in0, pn_repeat, uv + sh / nf, 0.0).rgb;
    }

    if k.smooth_life != 0 {
        // Smooth time: a moves towards the transition value; dt 1 is the discrete rule.
        let tv = smooth_life(u1.x, u0.x);
        a.x = mix(a.x, tv, k.dt);
    } else {
        let da = term(k.g0, k.i0, u0, u1, u2) + term(k.g1, k.i1, u0, u1, u2)
               + term(k.g2, k.i2, u0, u1, u2) + term(k.g3, k.i3, u0, u1, u2)
               + term(k.g4, k.i4, u0, u1, u2) + term(k.g5, k.i5, u0, u1, u2);
        a = a + k.dt * da;
    }
    a = a - vec3f(f.b * 0.2);
    // feed and kill act on the channels the rule uses.
    let chans = select(vec3f(1.0, 0.0, 0.0), vec3f(1.0), multi);
    if people == people_mode_feed { a = a + chans * (f.g * 0.05); }
    if people == people_mode_kill { a = a - chans * (f.g * 0.2); }

    // A stir: a few holes punched into a world that stood still for too long.
    if first && flags.b > 0.5 {
        let stirs = u32(textureLoad(in2, vec2i(3, 0), 0).a);
        for (var i = 0; i < params.stir_count; i++) {
            let c = rand2(stirs, u32(i)) * nf;
            var d = abs(vec2f(p) - c);
            d = min(d, nf - d);
            if length(d) < radius * 1.5 {
                a = vec3f(0.0);
            }
        }
    }
    a = clamp(a, vec3f(0.0), vec3f(1.0));
    return vec4f(a, 1.0);
}
