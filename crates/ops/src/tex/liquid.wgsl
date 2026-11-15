// tex.liquid: a 2D liquid with a free surface, MLS-MPM (moving least squares material point
// method) with quadratic B-spline weights and APIC transfers, weakly compressible: the pressure
// follows the density the grid gives each particle.
//
// Units are the picture's: positions in picture heights from the top left, y down, velocities
// in heights per second. Grid node i sits at (i - PAD) * dx, so the picture spans nodes PAD to
// PAD + its cells, and PAD nodes of margin lie around it. A particle's mass is 1; the rest
// density is `u.cell.w` masses per cell.
//
// Per frame: `fill` (after a start), `emit` (when an emitter pours), then per substep `p2g`
// (particles to grid), `grid_update` (gravity, the `force` picture in pic0, the splats, drag and
// the solids), `g2p` (grid to particles); last the pictures `density` and `velocity`. `recount` counts the particles in use after the buffer changed size. The
// liquid is drawn by a program of its own (`liquid_draw.wgsl`) from the particles.
//
// A particle's `mat` is 0 while it is free (a buffer of zeros is all free), else its material:
// 1 liquid, 2 sand, 3 snow. A liquid needs no deformation gradient, so its places (f0, f1) and
// `jp` hold what the drawing needs: the velocity smoothed over a frame or so, the density
// smoothed the same way, the foam, and a random number of the particle's own. Sand and snow
// carry their deformation gradient there, and in `jp` sand its volume gained by loosening
// (log), snow its plastic volume (1 at rest).
//
// Sand is Drucker-Prager plasticity on a Hencky (log strain) elastic law: it holds a heap up to
// its angle of repose, pours, and comes apart without pulling back. Snow is Stomakhin's model:
// fixed corotated elasticity whose stretch beyond a limit becomes permanent and hardens the snow
// where it is pressed together. Each material brings its own friction on solids; the grid keeps
// a node's share of it beside the mass, so water beside sand slides as freely as before.

const PAD: f32 = 3.0;
const FREE: u32 = 0u;
const MATERIALS: u32 = 4u;
const LIQUID: u32 = 1u;
const SAND: u32 = 2u;
const SNOW: u32 = 3u;
// A material's friction on solids is summed per node in units of this, so the sum stays within
// the node's mass in the fixed point.
const MOST_FRICTION: f32 = 10.0;
// Most density ratio sand's volume correction aims for.
const MAX_CROWD: f32 = 4.0;
// How far friction reaches out from a solid's surface, in cells.
const FRICTION_REACH: f32 = 1.0;
// Particles nearer a solid than this many cells add their material's friction to the grid: the
// nodes within FRICTION_REACH of it take their mass from no farther.
const FRICTION_NEAR: f32 = 4.0;
// Foam starts where a particle's acceleration beyond gravity rises from the first to the second
// value (an impact), in picture heights per second squared: 40 to 140 m/s^2 in a picture 3 m high.
const IMPACT_FROM: f32 = 13.333;
const IMPACT_TO: f32 = 46.667;
// Foam also starts in fast spray: faster than the first to the second speed (2 to 4 m/s), in
// heights per second, and thinner than the first to the second share of the rest density.
const SPRAY_FROM: f32 = 0.6667;
const SPRAY_TO: f32 = 1.3333;
const SPRAY_THICK: f32 = 0.25;
const SPRAY_THIN: f32 = 0.1;
// How much foam spray makes, against an impact.
const SPRAY_FOAM: f32 = 0.6;

// The counters, `counter[...]`.
const FREE_SEEN: u32 = 0u;   // free particles the emit pass saw this frame
const DRAINED: u32 = 1u;     // particles freed this frame
const WRAPPED: u32 = 2u;     // grid cells whose mass did not fit this frame
const FILL_WANTED: u32 = 3u; // particles the last fill asked for
const LIVE: u32 = 4u;        // particles in use
const PEAK: u32 = 5u;        // the force picture's strongest push where there is liquid, as float bits
const MOVING: u32 = 6u;      // the fastest obstacle, heights per second, as float bits
// Most splats after the emitters in their buffer.
const MAX_SPLATS: u32 = 64u;

struct Mat {
    // Friction on solids (Coulomb), mass of a particle, then liquid: sound speed squared over
    // gamma, gamma; sand and snow: the Lame numbers mu and lambda over the density.
    a: vec4f,
    // Liquid: viscosity, bulk viscosity, largest density ratio, most negative pressure (share
    // of a.z). Sand: friction coefficient alpha, cohesion (log strain), volume correction
    // (sound speed squared over gamma, 0 for none), unused. Snow: critical compression,
    // critical stretch, hardening, most hardening.
    b: vec4f,
}

// Constants of the frame, written by the CPU.
struct U {
    grid: vec4u,  // nodes across, nodes down, particles in the buffer, flags (bit 0: open edges; bit 1: the grid sums friction; bits 8 to 15: the fill's material)
    cell: vec4f,  // dx, 1 / dx, the substep's dt, rest mass per cell
    fixed: vec4f, // fixed point: mass scale, momentum scale; fastest speed on the grid; push out of solids
    wall: vec4f,  // friction rate, Coulomb friction, boundary band in cells, ghost density of solids
    force: vec4f, // gravity x, y; drag per second; largest momentum a particle adds, in vmax
    room: vec4f,  // the picture's width and height in heights; jitter of new particles; fill spacing
    emit: vec4u,  // emitters, particles they ask for, seed, fill lattice columns
    draw: vec4f,  // per substep: share the smoothed density moves, share of the foam kept, share the smoothed velocity moves; 1 / its time
    mats: array<Mat, 4>,
    push: vec4f,   // the force picture's gain (per second squared per height per input frame), the splats' (per second), the most push (heights per second squared), the most motion read
    pushes: vec4u, // the force picture read (pic0), splats, splats set the velocity, the first splat in `emitters`
}

struct P {
    pos: vec2f,
    vel: vec2f,
    c0: vec2f, // the affine velocity, columns
    c1: vec2f,
    f0: vec2f, // solids: the deformation gradient, columns. Liquid: the smoothed velocity
    f1: vec2f, //   and (smoothed density over the rest density, foam 0 to 1)
    rho: f32,  // density at the last gather
    sd: f32,   // signed distance to the nearest solid at the last gather
    jp: f32,   // liquid: a random number of its own, 0 to 1, for `grain`. Sand: volume gained (log). Snow: plastic volume
    mat: u32,
}

// One emitter: a row of `cols` nozzles across `dir`, pouring `rows` rows this frame. A splat
// after the emitters keeps its position and velocity in `a` (heights, heights per second) and its
// radius in `b.x`.
struct Em {
    a: vec4f, // x, y in heights; dir x, y (unit)
    b: vec4f, // speed, distance of the row poured nearest the nozzle, spacing, unused
    c: vec4u, // cols, first slot, material, rows
}

@group(2) @binding(0) var<uniform> u: U;
@group(2) @binding(1) var<storage, read_write> parts: array<P>;
// Per cell: mass, momentum x, momentum y, as fixed point; while sand or snow is in the liquid
// also the materials' friction on solids times their mass (over MOST_FRICTION). A liquid alone
// keeps three numbers per cell: the fourth would cost it a few percent.
@group(2) @binding(2) var<storage, read_write> ga: array<atomic<i32>>;
// Per cell: velocity, mass, signed distance to the nearest solid at this substep.
@group(2) @binding(3) var<storage, read_write> gf: array<vec4f>;
// Per cell: signed distance to the nearest solid (heights, negative inside), its velocity, its
// friction factor.
@group(2) @binding(4) var<storage, read> solid: array<vec4f>;
@group(2) @binding(5) var<storage, read_write> counter: array<atomic<u32>>;
@group(2) @binding(6) var<storage, read> emitters: array<Em>;

fn weights(fx: vec2f) -> array<vec2f, 3> {
    let a = vec2f(1.5) - fx;
    let b = fx - vec2f(1.0);
    let c = fx - vec2f(0.5);
    return array<vec2f, 3>(0.5 * a * a, vec2f(0.75) - b * b, 0.5 * c * c);
}

// Numbers per cell in `ga`.
fn sums() -> u32 {
    return select(3u, 4u, (u.grid.w & 2u) != 0u);
}

fn nodes() -> vec2i {
    return vec2i(u.grid.xy);
}

fn cell(base: vec2i, gx: i32, gy: i32) -> u32 {
    let c = clamp(base + vec2i(gx, gy), vec2i(0), nodes() - vec2i(1));
    return u32(c.y) * u.grid.x + u32(c.x);
}

fn node_at(c: vec2i) -> u32 {
    let q = clamp(c, vec2i(0), nodes() - vec2i(1));
    return u32(q.y) * u.grid.x + u32(q.x);
}

fn solid_at(c: vec2i) -> vec4f {
    return solid[node_at(c)];
}

// The distance to the nearest solid at grid coordinates `at`, interpolated.
fn solid_distance(at: vec2f) -> f32 {
    let f = floor(at);
    let t = at - f;
    let c = vec2i(f);
    let s00 = solid_at(c).x;
    let s10 = solid_at(c + vec2i(1, 0)).x;
    let s01 = solid_at(c + vec2i(0, 1)).x;
    let s11 = solid_at(c + vec2i(1, 1)).x;
    return mix(mix(s00, s10, t.x), mix(s01, s11, t.x), t.y);
}

// A particle's place in grid coordinates (node i at i).
fn grid_of(pos: vec2f) -> vec2f {
    return pos * u.cell.y + vec2f(PAD);
}

fn material(m: u32) -> Mat {
    return u.mats[min(max(m, 1u), MATERIALS) - 1u];
}

fn finite(v: vec2f) -> bool {
    // NaN fails the first test, infinity the second.
    return all(v == v) && all(abs(v) < vec2f(1e30));
}

// Liquid: pressure over density, and viscosity over the density ratio.
fn liquid(m: Mat, rho: f32, div: f32) -> vec2f {
    let r = clamp(rho / (u.cell.w * m.a.y), 0.2, m.b.z);
    var p = max(m.a.z * (pow(r, m.a.w) - 1.0), -m.b.w * m.a.z) / r;
    if (r > 0.8) {
        p -= m.b.y * div;
    }
    return vec2f(p, m.b.x / r);
}

fn rot(t: f32) -> mat2x2f {
    let c = cos(t);
    let s = sin(t);
    return mat2x2f(vec2f(c, s), vec2f(-s, c));
}

struct Svd {
    phi: f32,
    sig: vec2f,
    theta: f32,
}

// m = rot(phi) * diag(sig) * rot(theta), closed form for 2 x 2.
fn svd(m: mat2x2f) -> Svd {
    let e = 0.5 * (m[0].x + m[1].y);
    let f = 0.5 * (m[0].x - m[1].y);
    let g = 0.5 * (m[0].y + m[1].x);
    let h = 0.5 * (m[0].y - m[1].x);
    let q = sqrt(e * e + h * h);
    let r = sqrt(f * f + g * g);
    var a1 = 0.0;
    var a2 = 0.0;
    if (r > 1e-12) {
        a1 = atan2(g, f);
    }
    if (q > 1e-12) {
        a2 = atan2(h, e);
    }
    return Svd(0.5 * (a2 + a1), vec2f(q + r, q - r), 0.5 * (a2 - a1));
}

// Kirchhoff stress over the rest density of a sand or snow particle.
fn solid_stress(m: Mat, p: P) -> mat2x2f {
    let f = mat2x2f(p.f0, p.f1);
    var mu = m.a.z;
    var la = m.a.w;
    if (p.mat == SAND) {
        // Hencky strain in the principal frame.
        let s = svd(f);
        let e = log(max(abs(s.sig), vec2f(1e-4)));
        let t = 2.0 * mu * e + vec2f(la * (e.x + e.y));
        let uu = rot(s.phi);
        return uu * mat2x2f(vec2f(t.x, 0.0), vec2f(0.0, t.y)) * transpose(uu);
    }
    // Snow: stiffer where pressed together, softer where stretched.
    let h = clamp(exp(m.b.z * (1.0 - p.jp)), 0.05, m.b.w);
    mu *= h;
    la *= h;
    let r = rot(atan2(f[0].y - f[1].x, f[0].x + f[1].y));
    let j = determinant(f);
    let k = la * (j - 1.0) * j;
    return 2.0 * mu * (f - r) * transpose(f) + mat2x2f(vec2f(k, 0.0), vec2f(0.0, k));
}

@compute @workgroup_size(64)
fn p2g(@builtin(global_invocation_id) id: vec3u) {
    let i = id.x;
    if (i >= pn.size.x) {
        return;
    }
    if (parts[i].mat == FREE) {
        return;
    }
    let p = parts[i];
    // Inside a solid (one appeared around it): no part in the liquid until it is out again.
    // Counted, it would stack on the density the solid cells stand for and blast its
    // neighbours away.
    if (p.mat > MATERIALS || p.sd < -0.5 * u.cell.x) {
        return;
    }
    let m = material(p.mat);
    let mass = m.a.y;
    let dx = u.cell.x;
    let xg = grid_of(p.pos);
    let base = vec2i(floor(xg - vec2f(0.5)));
    let fx = xg - vec2f(base);
    let w = weights(fx);
    let k = u.cell.z * 4.0 * u.cell.y * u.cell.y;
    // Momentum the particle's stress gives a node at offset d: k * (st * d).
    var st: mat2x2f;
    if (p.mat == LIQUID) {
        // The density of the last gather, carried along.
        let s = liquid(m, p.rho, p.c0.x + p.c1.y);
        let cc = mat2x2f(p.c0, p.c1);
        st = mat2x2f(vec2f(s.x, 0.0), vec2f(0.0, s.x)) - s.y * (cc + transpose(cc));
    } else {
        st = -1.0 * solid_stress(m, p);
    }
    // The material's friction on solids, as a share of the most there is: only near a solid,
    // where it acts (a particle moves less than a cell a substep).
    let stride = sums();
    var rub = 0.0;
    if (stride == 4u && p.sd < FRICTION_NEAR * dx) {
        rub = clamp(m.a.x / MOST_FRICTION, 0.0, 1.0);
    }
    // A particle adds at most this much momentum to a node, per unit weight: a squeeze that
    // drives the pressure up cannot push a node's sum past what the fixed point holds.
    let most = u.force.w * u.fixed.z * mass * u.fixed.y;
    for (var gx = 0; gx < 3; gx++) {
        for (var gy = 0; gy < 3; gy++) {
            let wgt = w[gx].x * w[gy].y;
            let d = (vec2f(f32(gx), f32(gy)) - fx) * dx;
            var mv = (p.vel + p.c0 * d.x + p.c1 * d.y + k * (st * d)) * mass * u.fixed.y;
            mv = clamp(mv, vec2f(-most), vec2f(most)) * wgt;
            let c = stride * cell(base, gx, gy);
            atomicAdd(&ga[c], i32(round(wgt * mass * u.fixed.x)));
            atomicAdd(&ga[c + 1u], i32(round(mv.x)));
            atomicAdd(&ga[c + 2u], i32(round(mv.y)));
            if (rub > 0.0) {
                atomicAdd(&ga[c + 3u], i32(round(wgt * mass * rub * u.fixed.x)));
            }
        }
    }
}

// The velocity `v` of node `node` after a substep of `dt` under the `force` picture and the
// splats. Their push together is cut to `u.push.z`; a splat that sets the velocity does so after.
fn pushed(node: vec2u, v: vec2f, dt: f32) -> vec2f {
    let pos = (vec2f(node) - vec2f(PAD)) * u.cell.x;
    var a = vec2f(0.0);
    if (u.pushes.x != 0u) {
        // Motion in widths and heights per input frame, as tex.optical_flow gives it.
        var f = textureSampleLevel(pic0, pn_clamp, pos / u.room.xy, 0.0).xy;
        if (!finite(f)) {
            f = vec2f(0.0);
        }
        f = clamp(f, vec2f(-u.push.w), vec2f(u.push.w));
        a = vec2f(f.x * u.room.x, f.y) * u.push.x;
        // The strongest push, before the cut, for the substeps of the frames to come. Float bits
        // of numbers not below 0 order as the numbers do.
        let strength = min(length(a), 1e30);
        if (params.t == 0.0 && strength > bitcast<f32>(atomicLoad(&counter[PEAK]))) {
            atomicMax(&counter[PEAK], bitcast<u32>(strength));
        }
    }
    let n = min(u.pushes.y, MAX_SPLATS);
    if (u.pushes.z == 0u) {
        for (var k = 0u; k < n; k++) {
            let s = emitters[u.pushes.w + k];
            let d = pos - s.a.xy;
            let r = max(s.b.x, 1e-4);
            a += s.a.zw * exp(-dot(d, d) / (r * r)) * u.push.y;
        }
    }
    let strength = length(a);
    if (strength > u.push.z) {
        a *= u.push.z / strength;
    }
    var out = v;
    if (finite(a)) {
        out += a * dt;
    }
    if (u.pushes.z != 0u) {
        // A nozzle or a hand: the liquid inside the splat takes on its velocity.
        for (var k = 0u; k < n; k++) {
            let s = emitters[u.pushes.w + k];
            let d = pos - s.a.xy;
            let r = max(s.b.x, 1e-4);
            out = mix(out, s.a.zw, clamp(exp(-dot(d, d) / (r * r)), 0.0, 1.0));
        }
    }
    return out;
}

@compute @workgroup_size(8, 4)
fn grid_update(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= pn.size.xy)) {
        return;
    }
    let idx = id.y * u.grid.x + id.x;
    let stride = sums();
    let k = stride * idx;
    let mi = atomicLoad(&ga[k]);
    let mx = atomicLoad(&ga[k + 1u]);
    let my = atomicLoad(&ga[k + 2u]);
    var mr = 0;
    if (stride == 4u) {
        mr = atomicLoad(&ga[k + 3u]);
    }
    let dt = u.cell.z;
    var v = vec2f(0.0);
    var m = 0.0;
    // Coulomb friction on solids: the node's, or more where its sand or snow asks for more.
    var coulomb = u.wall.y;
    if (mi > 0) {
        m = f32(mi) / u.fixed.x;
        if (mr > 0) {
            coulomb = max(coulomb, f32(mr) / f32(mi) * MOST_FRICTION);
        }
        v = vec2f(f32(mx), f32(my)) / (u.fixed.y * m);
        v += u.force.xy * dt;
        if (u.pushes.x != 0u || u.pushes.y != 0u) {
            v = pushed(id.xy, v, dt);
        }
        v *= max(1.0 - u.force.z * dt, 0.0);
    } else if (mi < 0) {
        // More mass than the fixed point holds: the sum wrapped. The cell counts as empty and
        // still, so nothing takes a wrong velocity from it.
        atomicAdd(&counter[WRAPPED], 1u);
    }
    if (mi != 0 || mx != 0 || my != 0 || mr != 0) {
        atomicStore(&ga[k], 0);
        atomicStore(&ga[k + 1u], 0);
        atomicStore(&ga[k + 2u], 0);
        if (stride == 4u) {
            atomicStore(&ga[k + 3u], 0);
        }
    }

    // The solid field shows the solids at the end of the frame, `-params.t` seconds from now: a
    // moving solid is looked up where it goes.
    let so = solid[idx];
    var s = so.x;
    let vs = so.yz;
    if (params.t == 0.0 && (vs.x != 0.0 || vs.y != 0.0)) {
        // The fastest obstacle, for the substeps of the frames to come.
        let speed = min(length(vs), 1e30);
        if (speed > bitcast<f32>(atomicLoad(&counter[MOVING]))) {
            atomicMax(&counter[MOVING], bitcast<u32>(speed));
        }
    }
    var at = vec2f(id.xy);
    let dx = u.cell.x;
    if (s < 3.0 * dx) {
        if (vs.x != 0.0 || vs.y != 0.0) {
            at -= vs * params.t * u.cell.y;
            let f = floor(at);
            let t = at - f;
            let c = vec2i(f);
            let s00 = solid_at(c).x;
            let s10 = solid_at(c + vec2i(1, 0)).x;
            let s01 = solid_at(c + vec2i(0, 1)).x;
            let s11 = solid_at(c + vec2i(1, 1)).x;
            s = mix(mix(s00, s10, t.x), mix(s01, s11, t.x), t.y);
        }
        let band = u.wall.z * dx;
        // Friction reaches a cell beyond the band: the nodes beside a surface carry a film
        // along it, and a film held only at the band slides on over it.
        let reach = select(band, max(band, FRICTION_REACH * dx), coulomb > 0.0);
        if (s < reach) {
            let c = vec2i(round(at));
            let g = vec2f(
                solid_at(c + vec2i(1, 0)).x - solid_at(c - vec2i(1, 0)).x,
                solid_at(c + vec2i(0, 1)).x - solid_at(c - vec2i(0, 1)).x,
            );
            let len = length(g);
            if (s < -1.5 * dx || (len < 1e-9 && s < band)) {
                v = vs;
            } else if (len >= 1e-9) {
                let n = g / len;
                var vr = v - vs;
                let vn = dot(vr, n);
                if (vn < 0.0) {
                    // Within the band nothing goes into the solid; beyond it only friction acts.
                    let vt = vr - vn * n;
                    var kept = vt;
                    // Coulomb: what presses on the solid is held by up to that much.
                    let t = length(vt);
                    if (t > 1e-9) {
                        kept = vt * max(1.0 - coulomb * so.w * (-vn) / t, 0.0);
                    }
                    if (s < band) {
                        vr = kept;
                    } else {
                        vr = kept + vn * n;
                    }
                }
                // Friction as a rate: what slides along the solid loses this share per second.
                vr *= max(1.0 - u.wall.x * so.w * dt, 0.0);
                v = vr + vs;
            }
        }
    }
    let speed = length(v);
    if (speed > u.fixed.z) {
        v *= u.fixed.z / speed;
    }
    if (!finite(v)) {
        v = vec2f(0.0);
    }
    gf[idx] = vec4f(v, m, s);
}

@compute @workgroup_size(64)
fn g2p(@builtin(global_invocation_id) id: vec3u) {
    let i = id.x;
    if (i >= pn.size.x) {
        return;
    }
    if (parts[i].mat == FREE) {
        return;
    }
    var p = parts[i];
    let dx = u.cell.x;
    let xg = grid_of(p.pos);
    let base = vec2i(floor(xg - vec2f(0.5)));
    let fx = xg - vec2f(base);
    let w = weights(fx);
    var v = vec2f(0.0);
    var c0 = vec2f(0.0);
    var c1 = vec2f(0.0);
    var s = 0.0;
    var gs = vec2f(0.0);
    var rho = 0.0;
    for (var gx = 0; gx < 3; gx++) {
        for (var gy = 0; gy < 3; gy++) {
            let wgt = w[gx].x * w[gy].y;
            let d = (vec2f(f32(gx), f32(gy)) - fx) * dx;
            let g = gf[cell(base, gx, gy)];
            v += wgt * g.xy;
            c0 += wgt * g.xy * d.x;
            c1 += wgt * g.xy * d.y;
            // A solid counts as liquid at rest, or particles would crowd against it.
            rho += wgt * (g.z + clamp(0.5 - g.w * u.cell.y, 0.0, 1.0) * u.wall.w * u.cell.w);
            s += wgt * g.w;
            gs += wgt * g.w * d;
        }
    }
    let k = 4.0 * u.cell.y * u.cell.y;
    c0 *= k;
    c1 *= k;
    gs *= k;
    if (!finite(v) || !finite(c0) || !finite(c1)) {
        v = vec2f(0.0);
        c0 = vec2f(0.0);
        c1 = vec2f(0.0);
    }
    let dt = u.cell.z;
    let step = v * dt;
    var x = p.pos + step;
    // Out of a solid, by the distance field the grid carries.
    let sn = s + dot(gs, step);
    let gl = length(gs);
    if (sn < 0.0) {
        // Where the field has no slope (the middle of a symmetric obstacle, a picture that is
        // all solid) the particle goes up, so it does not stay inside for ever.
        var n = vec2f(0.0, -1.0);
        if (gl > 1e-9) {
            n = gs / gl;
        }
        let vs = solid_at(vec2i(round(xg))).yz;
        // Out at a bounded speed (on top of the solid's own): liquid that a solid appears in
        // leaves over a few frames instead of bursting out.
        x += n * min(-sn, (u.fixed.w + length(vs)) * dt);
        // A particle the grid let into a solid must not keep heading inwards: a node just
        // outside the solid is free, and a lone particle would fall on it for ever.
        let vn = dot(v - vs, n);
        if (vn < 0.0) {
            v -= vn * n;
        }
    }
    let room = u.room.xy;
    let open = (u.grid.w & 1u) != 0u;
    if (!finite(x) || (open && (any(x < vec2f(0.0)) || any(x > room)))) {
        // Out through an open edge, or lost: the particle is free for an emitter again.
        parts[i].mat = FREE;
        atomicAdd(&counter[DRAINED], 1u);
        atomicSub(&counter[LIVE], 1u);
        return;
    }
    let lo = vec2f(1.0 - PAD) * dx;
    let hi = (vec2f(u.grid.xy) - vec2f(2.0 + PAD)) * dx;
    p.pos = clamp(x, lo, hi);
    p.sd = sn;
    p.vel = v;
    p.c0 = c0;
    p.c1 = c1;
    // The density where the particle is going: this gather's, carried by the divergence.
    p.rho = rho * (1.0 - dt * (c0.x + c1.y));
    if (p.mat == LIQUID) {
        look(&p);
    } else if (p.mat <= MATERIALS) {
        deform(&p, mat2x2f(c0, c1) * dt);
    }
    parts[i] = p;
}

// Sand and snow carry their deformation on by the velocity gradient times the substep (`dc`),
// and what goes beyond the material's limits becomes permanent.
fn deform(p: ptr<function, P>, dc: mat2x2f) {
    let m = material((*p).mat);
    var f = (mat2x2f(vec2f(1.0, 0.0), vec2f(0.0, 1.0)) + dc) * mat2x2f((*p).f0, (*p).f1);
    if ((*p).mat == SAND && m.b.z > 0.0) {
        // Grains closer together than the deformation says (the grid lets particles drift
        // into clumps): the deformation takes a share of the difference in volume each
        // substep, so the sand pushes back elastically, within its friction, and keeps its
        // volume at rest.
        let r = clamp((*p).rho / (u.cell.w * m.a.y), 0.0, MAX_CROWD);
        let j = determinant(f);
        if (r > 1.0 && j * r > 1.0) {
            f *= pow(1.0 / (j * r), 0.5 * m.b.z);
        }
    }
    let sv = svd(f);
    if ((*p).mat == SAND) {
        // Back onto the Drucker-Prager cone, in log strain.
        let sig = max(abs(sv.sig), vec2f(1e-4));
        let e = log(sig);
        let tr = e.x + e.y + (*p).jp - 2.0 * m.b.y;
        var ns = sig;
        if (tr >= 0.0) {
            // Pulled apart: no stress, and the volume gained is kept to give back first.
            ns = vec2f(exp(m.b.y));
            (*p).jp = clamp(tr, 0.0, 10.0);
        } else {
            (*p).jp = 0.0;
            let eh = e - vec2f(0.5 * (e.x + e.y));
            let el = length(eh);
            let dg = el + (m.a.w + m.a.z) / m.a.z * tr * m.b.x;
            if (dg > 0.0 && el > 1e-9) {
                ns = exp(e - dg / el * eh);
            }
        }
        f = rot(sv.phi) * mat2x2f(vec2f(ns.x, 0.0), vec2f(0.0, ns.y)) * rot(sv.theta);
    } else {
        // Snow: stretch beyond the limits becomes permanent and moves the hardening.
        let most = 1.0 + m.b.y;
        var ns = clamp(sv.sig, vec2f(1.0 - m.b.x), vec2f(most));
        var jp = (*p).jp * (sv.sig.x * sv.sig.y) / max(ns.x * ns.y, 1e-6);
        // Packed until the hardening reaches its most: pressed further, snow stays elastic.
        // Without this a dropped block squashes flat like a piston.
        let packed = 1.0 - log(max(m.b.w, 1.0)) / max(m.b.z, 1e-3);
        if (!(jp >= packed)) {
            ns = clamp(sv.sig, vec2f(0.3), vec2f(most));
            jp = (*p).jp * (sv.sig.x * sv.sig.y) / max(ns.x * ns.y, 1e-6);
        }
        (*p).jp = clamp(select(1.0, jp, jp == jp), 0.3, 3.0);
        f = rot(sv.phi) * mat2x2f(vec2f(ns.x, 0.0), vec2f(0.0, ns.y)) * rot(sv.theta);
    }
    if (!finite(f[0]) || !finite(f[1])) {
        f = mat2x2f(vec2f(1.0, 0.0), vec2f(0.0, 1.0));
    }
    (*p).f0 = f[0];
    (*p).f1 = f[1];
}

// What the drawing keeps per liquid particle, moved on by one substep. Every share comes from a
// time constant and the substep's length, so the look is the same at any frame rate and `speed`.
fn look(p: ptr<function, P>) {
    let v = (*p).vel;
    // The velocity smoothed over u.draw.w seconds lags behind the velocity by the acceleration
    // times that time: what is left beyond gravity is an impact or a sudden turn.
    let vs = (*p).f0 + (v - (*p).f0) * u.draw.z;
    let acc = length((v - vs) * u.draw.w - u.force.xy);
    let ratio = (*p).rho / max(u.cell.w, 1e-6);
    let impact = smoothstep(IMPACT_FROM, IMPACT_TO, acc);
    let spray = smoothstep(SPRAY_FROM, SPRAY_TO, length(v)) * (1.0 - smoothstep(SPRAY_THIN, SPRAY_THICK, ratio));
    let foam = max((*p).f1.y * u.draw.y, max(impact, SPRAY_FOAM * spray));
    let calm = (*p).f1.x + (ratio - (*p).f1.x) * u.draw.x;
    var f1 = vec2f(clamp(calm, 0.0, 1000.0), clamp(foam, 0.0, 1.0));
    if (!finite(f1)) {
        f1 = vec2f(1.0, 0.0);
    }
    (*p).f0 = select(v, vs, finite(vs));
    (*p).f1 = f1;
}

fn hash(x: u32) -> u32 {
    var h = x * 747796405u + 2891336453u;
    h = ((h >> ((h >> 28u) + 4u)) ^ h) * 277803737u;
    return (h >> 22u) ^ h;
}

// Two numbers from -0.5 to 0.5 for `key`.
fn jitter(key: u32) -> vec2f {
    let h = hash(key);
    return vec2f(f32(h & 0xffffu), f32(h >> 16u)) / 65535.0 - vec2f(0.5);
}

// A new particle; `key` picks its random number.
fn fresh(mat: u32, pos: vec2f, vel: vec2f, key: u32) -> P {
    var p: P;
    p.pos = pos;
    p.vel = vel;
    p.c0 = vec2f(0.0);
    p.c1 = vec2f(0.0);
    p.rho = u.cell.w * material(mat).a.y;
    p.sd = 1.0;
    p.mat = mat;
    if (mat == LIQUID) {
        // At rest, calm, and its grain.
        p.f0 = vel;
        p.f1 = vec2f(1.0, 0.0);
        p.jp = f32(hash(key ^ 0x9e3779b9u) >> 8u) / 16777216.0;
    } else {
        // Undeformed; snow with its full volume.
        p.f0 = vec2f(1.0, 0.0);
        p.f1 = vec2f(0.0, 1.0);
        p.jp = select(0.0, 1.0, mat == SNOW);
    }
    return p;
}

// Once per frame while an emitter pours: free particles take the emitters' places.
@compute @workgroup_size(64)
fn emit(@builtin(global_invocation_id) id: vec3u) {
    let i = id.x;
    if (i >= pn.size.x || parts[i].mat != FREE) {
        return;
    }
    let slot = atomicAdd(&counter[FREE_SEEN], 1u);
    if (slot >= u.emit.y) {
        return;
    }
    // The emitter whose slots hold this one: the last whose first slot is not beyond it.
    var e = 0u;
    for (var k = 1u; k < u.emit.x; k++) {
        if (emitters[k].c.y <= slot) {
            e = k;
        }
    }
    let em = emitters[e];
    let cols = max(em.c.x, 1u);
    let local = slot - em.c.y;
    let col = local % cols;
    let row = local / cols;
    let j = jitter(slot + u.emit.z * 7919u) * u.room.z;
    let dir = em.a.zw;
    let across = vec2f(-dir.y, dir.x);
    let sp = em.b.z;
    let pos = em.a.xy + across * (f32(col) - 0.5 * f32(cols - 1u) + j.x) * sp
        + dir * (em.b.y + (f32(row) + j.y) * sp);
    parts[i] = fresh(clamp(em.c.z, LIQUID, SNOW), pos, dir * em.b.x, slot + u.emit.z * 7919u);
    atomicAdd(&counter[LIVE], 1u);
}

// After a start: a particle at every point of a lattice over the picture where the `fill`
// picture (pic0) is brighter than half and no obstacle is, as far as the buffer holds. The
// buffer is free before.
@compute @workgroup_size(16, 4)
fn fill(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= pn.size.xy)) {
        return;
    }
    let sp = u.room.w;
    let j = jitter(id.y * u.emit.w + id.x) * u.room.z;
    let pos = (vec2f(id.xy) + vec2f(0.5) + j) * sp;
    let uv = pos / u.room.xy;
    let c = textureSampleLevel(pic0, pn_clamp, uv, 0.0);
    // No liquid inside an obstacle.
    if (!(pn_luma(c.rgb) > 0.5) || solid_distance(grid_of(pos)) < 0.0) {
        return;
    }
    let k = atomicAdd(&counter[FILL_WANTED], 1u);
    if (k >= u.grid.z) {
        return;
    }
    let mat = clamp((u.grid.w >> 8u) & 255u, LIQUID, SNOW);
    parts[k] = fresh(mat, pos, vec2f(0.0), id.y * u.emit.w + id.x);
    atomicAdd(&counter[LIVE], 1u);
}

// After the buffer changed size: the particles in use, counted anew.
@compute @workgroup_size(64)
fn recount(@builtin(global_invocation_id) id: vec3u) {
    let i = id.x;
    if (i >= pn.size.x || parts[i].mat == FREE) {
        return;
    }
    atomicAdd(&counter[LIVE], 1u);
}

// The node's mass at grid coordinates `at`, interpolated, as a share of the rest mass.
fn fullness(at: vec2f) -> f32 {
    let f = floor(at);
    let t = at - f;
    let c = vec2i(f);
    let m00 = gf[node_at(c)].z;
    let m10 = gf[node_at(c + vec2i(1, 0))].z;
    let m01 = gf[node_at(c + vec2i(0, 1))].z;
    let m11 = gf[node_at(c + vec2i(1, 1))].z;
    return mix(mix(m00, m10, t.x), mix(m01, m11, t.x), t.y) / u.cell.w;
}

fn velocity_at(at: vec2f) -> vec2f {
    let f = floor(at);
    let t = at - f;
    let c = vec2i(f);
    let v00 = gf[node_at(c)].xy;
    let v10 = gf[node_at(c + vec2i(1, 0))].xy;
    let v01 = gf[node_at(c + vec2i(0, 1))].xy;
    let v11 = gf[node_at(c + vec2i(1, 1))].xy;
    return mix(mix(v00, v10, t.x), mix(v01, v11, t.x), t.y);
}

// `density`: how full each cell is, 1 at rest, as opaque grey at the grid's size.
@compute @workgroup_size(8, 8, 1)
fn density(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= pn.size.xy)) {
        return;
    }
    let pos = (vec2f(id.xy) + vec2f(0.5)) / vec2f(pn.size.xy) * u.room.xy;
    let full = clamp(fullness(grid_of(pos)), 0.0, 60000.0);
    textureStore(pic_out, id.xy, vec4f(vec3f(full), 1.0));
}

// How far the `velocity` picture reaches beyond the liquid, in cells, and the width of its
// fall-off there.
const REACH: i32 = 4;
const REACH_SIGMA: f32 = 1.5;

// `velocity`: red x in widths per second, green y in heights per second, blue the speed in
// heights per second, alpha how full the cell is (0 to 1), at the grid's size. Beyond the liquid
// the velocity of the liquid nearby goes on for a few cells and fades, so a look coloured by
// speed has no step at the liquid's edge.
@compute @workgroup_size(8, 8, 1)
fn velocity(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= pn.size.xy)) {
        return;
    }
    let pos = (vec2f(id.xy) + vec2f(0.5)) / vec2f(pn.size.xy) * u.room.xy;
    let at = grid_of(pos);
    let full = fullness(at);
    // Inside: the nodes' velocities weighed by their mass, so empty nodes pull nothing to 0.
    let f = floor(at);
    let t = at - f;
    let c = vec2i(f);
    var near = vec3f(0.0);
    for (var k = 0; k < 4; k++) {
        let o = vec2i(k & 1, k >> 1u);
        let g = gf[node_at(c + o)];
        let w = select(1.0 - t.x, t.x, o.x == 1) * select(1.0 - t.y, t.y, o.y == 1) * max(g.z, 0.0);
        near += vec3f(g.xy * w, w);
    }
    // Around: the same over a few cells, weighed by distance too.
    var far = vec3f(0.0);
    let centre = vec2i(round(at));
    for (var j = -REACH; j <= REACH; j++) {
        for (var i = -REACH; i <= REACH; i++) {
            let q = centre + vec2i(i, j);
            let g = gf[node_at(q)];
            let d = vec2f(q) - at;
            let w = exp(-dot(d, d) / (2.0 * REACH_SIGMA * REACH_SIGMA)) * max(g.z, 0.0);
            far += vec3f(g.xy * w, w);
        }
    }
    // Fades out where little liquid is near, so the reach has no edge of its own.
    var v = vec2f(0.0);
    if (far.z > 1e-6) {
        v = far.xy / far.z * clamp(far.z / u.cell.w, 0.0, 1.0);
    }
    if (near.z > 1e-6) {
        v = mix(v, near.xy / near.z, clamp(4.0 * full, 0.0, 1.0));
    }
    v = clamp(v, vec2f(-60000.0), vec2f(60000.0));
    if (!finite(v)) {
        v = vec2f(0.0);
    }
    let speed = min(length(v), 60000.0);
    textureStore(pic_out, id.xy, vec4f(v.x / u.room.x, v.y, speed, clamp(full, 0.0, 1.0)));
}
