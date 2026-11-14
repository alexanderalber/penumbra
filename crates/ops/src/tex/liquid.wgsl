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
// (particles to grid), `grid_update`, `g2p` (grid to particles); last the pictures `draw`,
// `density` and `velocity`. `recount` counts the particles in use after the buffer changed size.
//
// A particle's `mat` is 0 while it is free (a buffer of zeros is all free), 1 to 4 a material.
// Only material 1, liquid, is simulated; the deformation gradient (f0, f1) waits for solids.

const PAD: f32 = 3.0;
const FREE: u32 = 0u;
const MATERIALS: u32 = 4u;

// The counters, `counter[...]`.
const FREE_SEEN: u32 = 0u;   // free particles the emit pass saw this frame
const DRAINED: u32 = 1u;     // particles freed this frame
const WRAPPED: u32 = 2u;     // grid cells whose mass did not fit this frame
const FILL_WANTED: u32 = 3u; // particles the last fill asked for
const LIVE: u32 = 4u;        // particles in use

struct Mat {
    // kind (0 liquid), mass of a particle, sound speed squared over gamma, gamma.
    a: vec4f,
    // viscosity, bulk viscosity, largest density ratio, most negative pressure (share of a.z).
    b: vec4f,
}

// Constants of the frame, written by the CPU.
struct U {
    grid: vec4u,  // nodes across, nodes down, particles in the buffer, flags (bit 0: open edges)
    cell: vec4f,  // dx, 1 / dx, the substep's dt, rest mass per cell
    fixed: vec4f, // fixed point: mass scale, momentum scale; fastest speed on the grid; push out of solids
    wall: vec4f,  // friction rate, Coulomb friction, boundary band in cells, ghost density of solids
    force: vec4f, // gravity x, y; drag per second; largest momentum a particle adds, in vmax
    room: vec4f,  // the picture's width and height in heights; jitter of new particles; fill spacing
    emit: vec4u,  // emitters, particles they ask for, seed, fill lattice columns
    look: vec4f,  // the colour of `out`, straight alpha
    mats: array<Mat, 4>,
}

struct P {
    pos: vec2f,
    vel: vec2f,
    c0: vec2f, // the affine velocity, columns
    c1: vec2f,
    f0: vec2f, // the deformation gradient, columns (solids)
    f1: vec2f,
    rho: f32,  // density at the last gather
    sd: f32,   // signed distance to the nearest solid at the last gather
    jp: f32,   // liquid: age in seconds
    mat: u32,
}

// One emitter: a row of `cols` nozzles across `dir`, pouring `rows` rows this frame.
struct Em {
    a: vec4f, // x, y in heights; dir x, y (unit)
    b: vec4f, // speed, distance the newest row has travelled, spacing, unused
    c: vec4u, // cols, first slot, material, rows
}

@group(2) @binding(0) var<uniform> u: U;
@group(2) @binding(1) var<storage, read_write> parts: array<P>;
// Per cell: mass, momentum x, momentum y, as fixed point.
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
    // The density of the last gather, carried along.
    let s = liquid(m, p.rho, p.c0.x + p.c1.y);
    // Momentum the particle's stress gives a node at offset d: k * (st * d).
    let cc = mat2x2f(p.c0, p.c1);
    let st = mat2x2f(vec2f(s.x, 0.0), vec2f(0.0, s.x)) - s.y * (cc + transpose(cc));
    // A particle adds at most this much momentum to a node, per unit weight: a squeeze that
    // drives the pressure up cannot push a node's sum past what the fixed point holds.
    let most = u.force.w * u.fixed.z * mass * u.fixed.y;
    for (var gx = 0; gx < 3; gx++) {
        for (var gy = 0; gy < 3; gy++) {
            let wgt = w[gx].x * w[gy].y;
            let d = (vec2f(f32(gx), f32(gy)) - fx) * dx;
            var mv = (p.vel + p.c0 * d.x + p.c1 * d.y + k * (st * d)) * mass * u.fixed.y;
            mv = clamp(mv, vec2f(-most), vec2f(most)) * wgt;
            let c = 3u * cell(base, gx, gy);
            atomicAdd(&ga[c], i32(round(wgt * mass * u.fixed.x)));
            atomicAdd(&ga[c + 1u], i32(round(mv.x)));
            atomicAdd(&ga[c + 2u], i32(round(mv.y)));
        }
    }
}

@compute @workgroup_size(8, 4)
fn grid_update(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= pn.size.xy)) {
        return;
    }
    let idx = id.y * u.grid.x + id.x;
    let k = 3u * idx;
    let mi = atomicLoad(&ga[k]);
    let mx = atomicLoad(&ga[k + 1u]);
    let my = atomicLoad(&ga[k + 2u]);
    let dt = u.cell.z;
    var v = vec2f(0.0);
    var m = 0.0;
    if (mi > 0) {
        m = f32(mi) / u.fixed.x;
        v = vec2f(f32(mx), f32(my)) / (u.fixed.y * m);
        v += u.force.xy * dt;
        v *= max(1.0 - u.force.z * dt, 0.0);
    } else if (mi < 0) {
        // More mass than the fixed point holds: the sum wrapped. The cell counts as empty and
        // still, so nothing takes a wrong velocity from it.
        atomicAdd(&counter[WRAPPED], 1u);
    }
    if (mi != 0 || mx != 0 || my != 0) {
        atomicStore(&ga[k], 0);
        atomicStore(&ga[k + 1u], 0);
        atomicStore(&ga[k + 2u], 0);
    }

    // The solid field is `params.t` seconds old: a moving solid is looked up where it came from.
    let so = solid[idx];
    var s = so.x;
    let vs = so.yz;
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
        if (s < u.wall.z * dx) {
            let c = vec2i(round(at));
            let g = vec2f(
                solid_at(c + vec2i(1, 0)).x - solid_at(c - vec2i(1, 0)).x,
                solid_at(c + vec2i(0, 1)).x - solid_at(c - vec2i(0, 1)).x,
            );
            let len = length(g);
            if (s < -1.5 * dx || len < 1e-9) {
                v = vs;
            } else {
                let n = g / len;
                var vr = v - vs;
                let vn = dot(vr, n);
                if (vn < 0.0) {
                    vr -= vn * n;
                    // Coulomb: what presses on the solid is held by up to that much.
                    let t = length(vr);
                    if (t > 1e-9) {
                        vr *= max(1.0 - u.wall.y * so.w * (-vn) / t, 0.0);
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
    if (sn < 0.0 && gl > 1e-9) {
        let n = gs / gl;
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
    p.jp += dt;
    parts[i] = p;
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

fn fresh(mat: u32, pos: vec2f, vel: vec2f) -> P {
    var p: P;
    p.pos = pos;
    p.vel = vel;
    p.c0 = vec2f(0.0);
    p.c1 = vec2f(0.0);
    p.f0 = vec2f(1.0, 0.0);
    p.f1 = vec2f(0.0, 1.0);
    p.rho = u.cell.w * material(mat).a.y;
    p.sd = 1.0;
    p.jp = 0.0;
    p.mat = mat;
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
    parts[i] = fresh(max(em.c.z, 1u), pos, dir * em.b.x);
    atomicAdd(&counter[LIVE], 1u);
}

// After a start: a particle at every point of a lattice over the picture where the `fill`
// picture (pic0) is brighter than half, as far as the buffer holds. The buffer is free before.
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
    if (!(pn_luma(c.rgb) > 0.5)) {
        return;
    }
    let k = atomicAdd(&counter[FILL_WANTED], 1u);
    if (k >= u.grid.z) {
        return;
    }
    parts[k] = fresh(1u, pos, vec2f(0.0));
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

// `out`: the grid's mass, upsampled and thresholded, in one colour. A stand-in for a drawing of
// the particles.
@compute @workgroup_size(8, 8, 1)
fn draw(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= pn.size.xy)) {
        return;
    }
    let uv = (vec2f(id.xy) + vec2f(0.5)) / vec2f(pn.size.xy);
    let full = fullness(grid_of(uv * u.room.xy));
    let a = smoothstep(0.2, 0.45, full) * clamp(u.look.a, 0.0, 1.0);
    let rgb = max(u.look.rgb, vec3f(0.0));
    textureStore(pic_out, id.xy, vec4f(rgb * a, a));
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

// `velocity`: red x in widths per second, green y in heights per second, at the grid's size.
@compute @workgroup_size(8, 8, 1)
fn velocity(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= pn.size.xy)) {
        return;
    }
    let pos = (vec2f(id.xy) + vec2f(0.5)) / vec2f(pn.size.xy) * u.room.xy;
    let v = clamp(velocity_at(grid_of(pos)), vec2f(-60000.0), vec2f(60000.0));
    textureStore(pic_out, id.xy, vec4f(v.x / u.room.x, v.y, 0.0, 1.0));
}
