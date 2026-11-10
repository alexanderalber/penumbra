// Slow growth: frost, branching (DLA-like) and lichen. One simulation step per pass; the
// node's `iterations` is the number of steps per frame (the component's steps times speed).
//
// `model` picks what grows; the params below set how. The component's presets write them.
//
// State texture (32-bit floats), by model:
//   frost:  r phase (0 open, 1 ice), g temperature (1 is the melting point), b the tick it
//           froze, a 1 + crystal orientation (0..1 of one arm sector)
//   branch: r 1 in the tree, g food (the diffusing field), b birth tick, a 1
//   lichen: r colony hue (0 empty, below 0 dead crust), g a random shade, b birth tick, a 1
// A tick is 1/7.5 s (8 frames at 60 fps). Ticks count from the last start: `now` is bound to
// the component's clock, which starts over with the simulation.
//
// in0: the state. in1: the object mask (seeds along its edges, or a wall).
// in2: heat (a tracked person): frost melts, branches and lichen die where it is bright.
// in3: seed dots: growth starts where it is bright (with dot_seeds on).
// All three may have any size; they are sampled at the simulation's texels.
//
// @param model choice(frost|branch|lichen) = frost -- follows the component's model: what grows
// @param area choice(all|inside|outside) = all -- follows the component's area: inside: growth only where in1 is bright; outside: only where it is dark
// @param now float = 0 0..100000 -- seconds since the start; bound to the component's clock
// @param seed int = 1 0..1000 -- follows the component's seed: other random places
// @param pause bool = false -- holds the state
// @param heat float = 1 0..2 -- how strongly in2 melts frost and kills branches and lichen; 0: people do nothing
// @param afterglow float = 0 0..3 -- warmth a person leaves behind: 0: the melted trail freezes again within seconds, with fingers; 1: slower, with smooth fronts; 2: it stays open for many seconds
// @param edge_seeds float = 0.03 0..1 -- density of seed points along the edges of in1
// @param dot_seeds bool = false -- seeds where in3 is bright
// @param spawn float = 0.03 0..50 -- new seeds appearing by themselves, per million texels per frame
// @param arms int = 6 2..12 -- frost: arms of a crystal
// @param aniso float = 0.05 0..0.1 -- frost: how strongly growth keeps to the arm directions
// @param latent float = 2.2 0.8..3 -- frost: heat set free by freezing; higher grows slower with thinner arms
// @param noise float = 0.3 0..0.5 -- frost: random kicks at the front: more side branches
// @param dt float = 0.2 0.01..0.3 -- frost: time step as dt / dx^2; above 0.25 the arms lock to the grid, above 0.37 it blows up
// @param zoom float = 1 0.5..3 -- frost: texels per cell of the model: bigger, slower crystals
// @param cool float = 0.3 0..20 -- frost: how fast the temperature returns to ambient (0: Kobayashi's closed box)
// @param icecool float = 6 0..20 -- frost: how much faster ice cools than open glass
// @param ambient float = 0 -0.5..2 -- frost: temperature of the surface; 1 is the melting point, above melts
// @param spell float = 60 0..600 -- frost: seconds per warm and cold spell (0: none)
// @param spell_depth float = 1.5 0..3 -- frost: how strongly a warm spell melts: 1.5 melts nearly all
// @param eta float = 1.5 0..6 -- branch: how much tips are preferred: 0 dense blobs, 1 coral, 3 lightning
// @param grow float = 0.05 0..1 -- branch: chance per step that a cell next to the tree joins it, at full food
// @param supply float = 0.00002 0..0.002 -- branch: how fast food comes back; small: long thin branches
// @param lifespan float = 0 0..250 -- branch and lichen: seconds a cell lives (0: forever)
// @param spread float = 0.02 0..0.3 -- lichen: chance per step that an empty cell next to a colony is taken
// @param ragged float = 0.8 0..1 -- lichen: how lumpy the ground is: ragged, lobed rims
// @param recover float = 20 0..250 -- lichen: seconds dead crust stays before it can be grown over again

struct Cfg {
    model: i32,
    area: i32,
    edge: f32,
    dots: f32,
    spawn: f32,
    arms: f32,
    aniso: f32,
    latent: f32,
    noise: f32,
    dt: f32,
    zoom: f32,
    cool: f32,
    icecool: f32,
    ambient: f32,
    spell: f32,
    depth: f32,
    heat: f32,
    eta: f32,
    grow: f32,
    supply: f32,
    life: f32,
    spread: f32,
    ragged: f32,
    recover: f32,
}

const TAU = 0.0003;
const EPSB = 0.01;
const ALPHA = 0.9;
const GAMMA = 10.0;
const TEQ = 1.0;

var<private> n: vec2i;

// The params as one struct, so the model functions read them in one place.
fn cfg() -> Cfg {
    return Cfg(params.model, params.area, params.edge_seeds, f32(params.dot_seeds), params.spawn,
               f32(params.arms), params.aniso, params.latent, params.noise, params.dt, params.zoom,
               params.cool, params.icecool, params.ambient, params.spell, params.spell_depth,
               params.heat, params.eta, params.grow, params.supply, params.lifespan,
               params.spread, params.ragged, params.recover);
}

fn ld(q: vec2i) -> vec4f {
    return textureLoad(in0, clamp(q, vec2i(0), n - vec2i(1)), 0);
}

fn salt() -> u32 {
    return u32(params.seed) * 2654435761u;
}

// Random 0..1, different per texel, pass and frame.
fn rnd(q: vec2i, k: u32) -> f32 {
    let h = pn_hash(vec3u(vec2u(q), u32(pn_time.frame) * 64u + u32(pn.iteration) * 4u + k * 1000003u + salt()));
    return f32(h.x >> 8u) / 16777216.0;
}

// Random 0..1 that stays the same over time.
fn fixed(q: vec2i, k: u32) -> f32 {
    let h = pn_hash(vec3u(vec2u(q + vec2i(4096)), k + salt()));
    return f32(h.x >> 8u) / 16777216.0;
}

// A factor 0.7..1.3 that varies smoothly over the ground (features about 40 texels).
fn smooth_var(q: vec2i, k: u32) -> f32 {
    return 1.0 + 0.3 * clamp(pn_noise(vec3f(vec2f(q) / 40.0, 0.0), k + salt()) * 1.5, -1.0, 1.0);
}

// Ticks since the start. Not wrapped: 32-bit floats count them exactly for days.
fn tick() -> f32 {
    return floor(params.now * 7.5);
}

fn age(birth: f32) -> f32 {
    return max(tick() - birth, 0.0);
}

fn luma_at(t: texture_2d<f32>, uv: vec2f) -> f32 {
    return clamp(pn_luma(textureSampleLevel(t, pn_clamp, uv, 0.0).rgb), 0.0, 1.0);
}

fn ang(g: vec2f) -> f32 {
    // atan2(0, 0) is NaN on D3D12: flat places get angle 0 (their gradient is 0 anyway).
    return select(0.0, atan2(g.y, g.x), dot(g, g) > 1e-8);
}

// eps * deps/dtheta; multiplied by a gradient component by the caller.
fn epsd(g: vec2f, th0: f32, k: Cfg) -> f32 {
    let th = ang(g);
    let e = EPSB * (1.0 + k.aniso * cos(k.arms * (th - th0)));
    let ed = -EPSB * k.arms * k.aniso * sin(k.arms * (th - th0));
    return e * ed;
}

// Is this texel a seed this frame? Seeds come from the edges of in1, bright dots in in3 and
// spontaneous ones. Returns 0 for none, else 1 + a random number for the seed's colour or
// orientation (the same over a 3 x 3 block).
fn seeding(q: vec2i, uv: vec2f, k: Cfg) -> f32 {
    var s = 0.0;
    if pn.iteration != 0 {
        return s;
    }
    let blk = q / 3;
    let o = 1.5 / vec2f(n);
    if k.edge > 0.0 {
        let a = luma_at(in1, uv + vec2f(o.x, 0.0));
        let b = luma_at(in1, uv - vec2f(o.x, 0.0));
        let c = luma_at(in1, uv + vec2f(0.0, o.y));
        let d = luma_at(in1, uv - vec2f(0.0, o.y));
        let edge = max(max(a, b), max(c, d)) > 0.5 && min(min(a, b), min(c, d)) < 0.5;
        // Fixed seed points along the edge; each comes back now and then when it is gone.
        if edge && fixed(blk, 7u) < k.edge * 0.25 && rnd(blk, 8u) < 0.03 {
            s = 1.0 + fixed(blk, 9u);
        }
    }
    if k.dots > 0.5 && luma_at(in3, uv) > 0.5 && rnd(blk, 10u) < 0.05 {
        s = 1.0 + fixed(blk, 11u);
    }
    if k.spawn > 0.0 && rnd(blk, 12u) < k.spawn * 9e-6 {
        s = 1.0 + rnd(blk, 13u);
    }
    return s;
}

fn allowed(uv: vec2f, k: Cfg) -> bool {
    var ok = true;
    if k.area == area_inside {
        ok = luma_at(in1, uv) > 0.5;
    } else if k.area == area_outside {
        ok = luma_at(in1, uv) <= 0.5;
    }
    return ok;
}

// ---------------------------------------------------------------- frost
fn frost(q: vec2i, uv: vec2f, k: Cfg) -> vec4f {
    let c = ld(q);
    let dx = 0.03 / max(k.zoom, 0.1);
    let dt = k.dt * dx * dx;
    let th0 = (c.a - 1.0) * 6.2831853 / k.arms;

    let e = ld(q + vec2i(1, 0));
    let w = ld(q + vec2i(-1, 0));
    let no = ld(q + vec2i(0, -1));
    let so = ld(q + vec2i(0, 1));
    let ne = ld(q + vec2i(1, -1));
    let nw = ld(q + vec2i(-1, -1));
    let se = ld(q + vec2i(1, 1));
    let sw = ld(q + vec2i(-1, 1));
    let ee = ld(q + vec2i(2, 0)).r;
    let ww = ld(q + vec2i(-2, 0)).r;
    let nn = ld(q + vec2i(0, -2)).r;
    let ss = ld(q + vec2i(0, 2)).r;

    let i2 = 1.0 / (2.0 * dx);
    // Nine-point Laplacians of phase and temperature, written as sums of differences so that
    // a flat field gives exactly 0 (else rounding makes heat out of nothing in 32-bit floats).
    let sides = (e - c) + (w - c) + (no - c) + (so - c);
    let corners = (ne - c) + (nw - c) + (se - c) + (sw - c);
    let lap = (4.0 * sides + corners) / (6.0 * dx * dx);
    let g = vec2f(e.r - w.r, so.r - no.r) * i2;
    let th = ang(g);
    let eps = EPSB * (1.0 + k.aniso * cos(k.arms * (th - th0)));
    // Kobayashi's anisotropy terms: d/dy(eps eps' dp/dx) - d/dx(eps eps' dp/dy).
    let gE = vec2f(ee - c.r, se.r - ne.r) * i2;
    let gW = vec2f(c.r - ww, sw.r - nw.r) * i2;
    let gN = vec2f(ne.r - nw.r, c.r - nn) * i2;
    let gS = vec2f(se.r - sw.r, ss - c.r) * i2;
    let t1 = (epsd(gS, th0, k) * gS.x - epsd(gN, th0, k) * gN.x) * i2;
    let t2 = -(epsd(gE, th0, k) * gE.y - epsd(gW, th0, k) * gW.y) * i2;

    let p = c.r;
    let T = c.g;
    let m = ALPHA / 3.14159265 * atan(GAMMA * (TEQ - T));
    let kick = k.noise * (rnd(q, 1u) - 0.5);
    let dp = dt / TAU * (t1 + t2 + eps * eps * lap.r + p * (1.0 - p) * (p - 0.5 + m + kick));
    var p2 = clamp(p + dp, 0.0, 1.0);
    // Ice gives its heat to the cold glass faster than open glass does (factor 1 + icecool):
    // tips still grow into slowly cooling open space and branch, but the heat stored in old
    // ice does not keep a melted place warm.
    let rate = k.cool * (1.0 + k.icecool * p);
    var T2 = T + dt * lap.g + k.latent * (p2 - p) + dt * rate * (k.ambient - T);

    // Melting. Warmth comes from the warm spells and from in2 (a person); below 0.1 it does
    // nothing. It thins the ice where it is: young, thin ice first, the old spines of the
    // crystals last, so melting runs the growth backwards.
    var warm = 0.0;
    if k.spell > 0.0 {
        // A smooth pulse in the middle of each cycle, about a tenth of it long. It runs on
        // the component's clock, so a restart begins with a cold cycle.
        let ph = fract(params.now / k.spell) - 0.5;
        warm += k.depth * exp(-ph * ph / 0.0036);
    }
    let h = luma_at(in2, uv) * k.heat;
    warm += 2.0 * h;
    // How thick the ice is, from its age. Ice that is already thinned (phase below 0.95)
    // counts as thin. Counted by age alone, the oldest ice resisted each warm spell, came out
    // of it older still and resisted the next one better: a band of ice along the picture's
    // border then survived every spell.
    let thick = sqrt(clamp(age(c.b) / 450.0, 0.0, 1.0)) * smoothstep(0.5, 0.95, p);
    let p3 = max(p2 - dt / TAU * 0.2 * max(warm - 0.1, 0.0) / (0.3 + 3.0 * thick), 0.0);
    // Melting takes back the heat that freezing gave: after a warm spell the glass is as cold
    // as before, and the next crystals branch as finely as the first.
    // (Not below ambient: the glass does not get colder than its surroundings.)
    if p3 < p2 {
        T2 = max(T2 + k.latent * (p3 - p2), min(T2, k.ambient));
    }
    p2 = p3;
    // A person leaves the glass at a temperature between ambient (afterglow 0) and the melting
    // point (afterglow 1). Cold glass behind a person freezes again within seconds, and the
    // fronts growing into it from the trail's edges break up into fingers. Glass near the
    // melting point freezes again slowly, as smooth fronts that leave a plain scar.
    if h > 0.001 {
        T2 = mix(T2, k.ambient + (1.0 - k.ambient) * params.afterglow, min(0.1 * h, 1.0));
    }

    // Orientation: open texels take the orientation of their most solid neighbour.
    var o = c.a;
    if p < 0.5 {
        var best = p;
        if e.r > best { best = e.r; o = e.a; }
        if w.r > best { best = w.r; o = w.a; }
        if no.r > best { best = no.r; o = no.a; }
        if so.r > best { best = so.r; o = so.a; }
    }

    // New crystals, only where it is cold.
    let s = seeding(q, uv, k);
    if s > 0.0 && T2 < 0.8 && p2 < 0.5 && warm < 0.1 {
        p2 = 1.0;
        o = s;
    }
    if !allowed(uv, k) {
        p2 = 0.0;
    }

    // b of ice is the tick it froze; show.wgsl turns its age into thickness and spines.
    var b = c.b;
    if p < 0.5 && p2 >= 0.5 {
        b = tick();
    }
    return vec4f(p2, T2, b, o);
}

// ---------------------------------------------------------------- branch (DLA-like)
fn branch(q: vec2i, uv: vec2f, k: Cfg) -> vec4f {
    let c = ld(q);
    let now = tick();
    var r = c;
    let h = luma_at(in2, uv) * k.heat;
    if c.r > 0.5 {
        // In the tree: no food here. Old or heated branches die.
        r = vec4f(1.0, 0.0, c.b, 1.0);
        var die = h > 0.3 && rnd(q, 3u) < 0.05 * h;
        if k.life > 0.0 && pn.iteration == 0 {
            let lim = k.life * 7.5 * (0.7 + 0.6 * fixed(q / 4, 5u));
            die = die || (age(c.b) > lim && rnd(q, 4u) < 0.05);
        }
        if die || !allowed(uv, k) {
            r = vec4f(0.0, 0.0, 0.0, 1.0);
        }
    } else {
        // Nine-point Laplacian of the food as a sum of differences (exactly 0 when flat).
        // Tree cells count as food 0: the tree eats what reaches it.
        var lap = 0.0;
        var touch = 0.0;
        for (var y = -1; y <= 1; y++) {
            for (var x = -1; x <= 1; x++) {
                if x != 0 || y != 0 {
                    let t = ld(q + vec2i(x, y));
                    let wgt = select(1.0, 4.0, x == 0 || y == 0);
                    lap += wgt * (select(t.g, 0.0, t.r > 0.5) - c.g);
                    touch += select(0.0, wgt, t.r > 0.5);
                }
            }
        }
        var f = c.g + 0.24 * lap / 6.0;
        f += k.supply * (1.0 - f);
        f = clamp(f, 0.0, 1.0);
        r = vec4f(0.0, f, 0.0, 1.0);
        // Join the tree: the chance rises with the food here, to the power eta.
        if touch > 0.0 && allowed(uv, k) && h < 0.3 {
            let pj = k.grow * pow(f, k.eta) * min(touch / 4.0, 1.0);
            if rnd(q, 6u) < pj {
                r = vec4f(1.0, 0.0, now, 1.0);
            }
        }
        let s = seeding(q, uv, k);
        if s > 0.0 && allowed(uv, k) && h < 0.3 {
            r = vec4f(1.0, 0.0, now, 1.0);
        }
    }
    return r;
}

// ---------------------------------------------------------------- lichen
fn lichen(q: vec2i, uv: vec2f, k: Cfg) -> vec4f {
    let c = ld(q);
    let now = tick();
    var r = c;
    let h = luma_at(in2, uv) * k.heat;
    if c.r < 0.0 {
        // Dead crust: becomes bare ground again after recover seconds.
        if pn.iteration == 0 && age(c.b) > k.recover * 7.5 * smooth_var(q, 21u) {
            r = vec4f(0.0, 0.0, 0.0, 1.0);
        }
    } else if c.r > 0.0 {
        var die = h > 0.3 && rnd(q, 3u) < 0.05 * h;
        if k.life > 0.0 && pn.iteration == 0 {
            // Old parts die. Ages run in rings, so a colony dies from its centre outwards; the
            // lifespan varies smoothly over the ground, so the dying edge is ragged too.
            die = die || age(c.b) > k.life * 7.5 * smooth_var(q, 22u);
        }
        if die || !allowed(uv, k) {
            r = vec4f(-1.0, c.g, now, 1.0);
        }
    } else if allowed(uv, k) && h < 0.3 {
        // Bare ground: taken from a random living neighbour, faster on good ground.
        var count = 0.0;
        var pick = 0.0;
        for (var y = -1; y <= 1; y++) {
            for (var x = -1; x <= 1; x++) {
                if x != 0 || y != 0 {
                    let t = ld(q + vec2i(x, y)).r;
                    if t > 0.0 {
                        count += 1.0;
                        if rnd(q + vec2i(x * 7, y * 13), 24u) * count < 1.0 {
                            pick = t;
                        }
                    }
                }
            }
        }
        if count > 0.0 {
            let fq = vec2f(q);
            let ground = 0.5 + 0.5 * pn_noise(vec3f(fq / 24.0, 0.0), 3u + salt()) + 0.25 * pn_noise(vec3f(fq / 7.0, 1.0), 4u + salt());
            let good = mix(1.0, clamp(ground * 1.4, 0.0, 1.5), k.ragged);
            if rnd(q, 25u) < k.spread * good * (0.4 + 0.6 * count / 3.0) {
                r = vec4f(pick, fixed(q, 26u), now, 1.0);
            }
        }
        let s = seeding(q, uv, k);
        if s > 0.0 {
            r = vec4f(0.02 + 0.98 * (s - 1.0), fixed(q, 26u), now, 1.0);
        }
    }
    return r;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    n = vec2i(textureDimensions(in0));
    let q = vec2i(v.pos.xy);
    let uv = (vec2f(q) + 0.5) / vec2f(n);
    let k = cfg();
    var r = ld(q);
    if params.pause == 0u {
        if k.model == model_frost {
            r = frost(q, uv, k);
        } else if k.model == model_branch {
            r = branch(q, uv, k);
        } else {
            r = lichen(q, uv, k);
        }
    }
    return r;
}
