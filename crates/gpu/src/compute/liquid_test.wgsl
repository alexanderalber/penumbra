// 2D MLS-MPM liquid, a test of compute programs with storage buffers: weakly compressible,
// density from the grid, APIC transfers, one scatter per substep.
//
// Positions are metres in the padded grid (the room's origin sits at pad * dx), velocities m/s.
// Node i sits at i * dx. A particle's mass is 1; rest density is `ppc` masses per cell.
// Per substep three dispatches: `p2g` (particles to grid, over the particles), `grid_update`
// (over the grid), `g2p` (grid to particles, over the particles). `pn.size` holds the count of
// each dispatch.

struct U {
    grid: vec2u,
    dx: f32,
    inv_dx: f32,
    dt: f32,
    c2g: f32, // sound speed squared over gamma
    gamma: f32,
    visc: f32, // kinematic viscosity, m^2/s
    ppc: f32,
    mscale: f32, // fixed point: mass
    pscale: f32, // fixed point: momentum
    vmax: f32,
    friction: f32,
    bc_band: f32, // nodes with a distance below this many cells get the solid's boundary condition
    gravity: vec2f,
    push: f32, // speed at which a particle leaves a solid it is inside of, m/s
    rmax: f32, // density ratio at which the pressure stops growing
    damp: f32, // velocity loss per second on the grid
    pneg: f32, // most negative pressure allowed, as a fraction of c2g (0: none)
    bulk: f32, // bulk viscosity, m^2/s: damps compression waves
    ghost: f32, // how much of the rest density a solid cell counts as, for a particle's density
}

struct P {
    pos: vec2f,
    vel: vec2f,
    c0: vec2f,
    c1: vec2f,
    rho: f32,
    sd: f32, // signed distance to the nearest solid at the last gather
}

// Constants of the frame, written by the CPU. A uniform: every thread reads them, which is
// faster from a uniform than from a storage buffer.
@group(2) @binding(0) var<uniform> u: U;
@group(2) @binding(1) var<storage, read_write> parts: array<P>;
// Per cell: mass, momentum x, momentum y, as fixed point.
@group(2) @binding(2) var<storage, read_write> ga: array<atomic<i32>>;
// Per cell: velocity, mass, signed distance to the nearest solid at this substep.
@group(2) @binding(3) var<storage, read_write> gf: array<vec4f>;
// Per cell, written by the CPU: signed distance (m), the solid's velocity, unused.
@group(2) @binding(4) var<storage, read> solid: array<vec4f>;

fn weights(fx: vec2f) -> array<vec2f, 3> {
    let a = vec2f(1.5) - fx;
    let b = fx - vec2f(1.0);
    let c = fx - vec2f(0.5);
    return array<vec2f, 3>(0.5 * a * a, vec2f(0.75) - b * b, 0.5 * c * c);
}

fn cell(base: vec2i, gx: i32, gy: i32) -> u32 {
    let c = clamp(base + vec2i(gx, gy), vec2i(0), vec2i(u.grid) - vec2i(1));
    return u32(c.y) * u.grid.x + u32(c.x);
}

// Pressure over density, and viscosity over the density ratio.
fn stress(rho: f32, div: f32) -> vec2f {
    let r = clamp(rho / u.ppc, 0.2, u.rmax);
    var p = max(u.c2g * (pow(r, u.gamma) - 1.0), -u.pneg * u.c2g) / r;
    if (r > 0.8) {
        p -= u.bulk * div;
    }
    return vec2f(p, u.visc / r);
}

// Momentum a particle's stress gives the node at offset d, per unit weight.
fn force(p: P, d: vec2f, s: vec2f) -> vec2f {
    let sym = p.c0 * d.x + p.c1 * d.y + vec2f(dot(p.c0, d), dot(p.c1, d));
    return u.dt * 4.0 * u.inv_dx * u.inv_dx * (s.x * d - s.y * sym);
}

@compute @workgroup_size(64)
fn p2g(@builtin(global_invocation_id) id: vec3u) {
    let i = id.x;
    if (i >= pn.size.x) {
        return;
    }
    let p = parts[i];
    // Inside a solid (one appeared around it): no part in the fluid until it is out again.
    // Counted, it would stack on the density the solid cells stand for and blast its
    // neighbours away.
    if (p.sd < -0.5 * u.dx) {
        return;
    }
    let xg = p.pos * u.inv_dx;
    let base = vec2i(floor(xg - vec2f(0.5)));
    let fx = xg - vec2f(base);
    let w = weights(fx);
    // The density of the last gather, carried along.
    let s = stress(p.rho, p.c0.x + p.c1.y);
    for (var gx = 0; gx < 3; gx++) {
        for (var gy = 0; gy < 3; gy++) {
            let wgt = w[gx].x * w[gy].y;
            let d = (vec2f(f32(gx), f32(gy)) - fx) * u.dx;
            let mv = (p.vel + p.c0 * d.x + p.c1 * d.y + force(p, d, s)) * wgt * u.pscale;
            let k = 3u * cell(base, gx, gy);
            atomicAdd(&ga[k], i32(round(wgt * u.mscale)));
            atomicAdd(&ga[k + 1u], i32(round(mv.x)));
            atomicAdd(&ga[k + 2u], i32(round(mv.y)));
        }
    }
}

fn solid_at(c: vec2i) -> vec4f {
    let q = clamp(c, vec2i(0), vec2i(u.grid) - vec2i(1));
    return solid[u32(q.y) * u.grid.x + u32(q.x)];
}

@compute @workgroup_size(8, 8)
fn grid_update(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= pn.size.xy)) {
        return;
    }
    let idx = id.y * u.grid.x + id.x;
    let k = 3u * idx;
    let mi = atomicLoad(&ga[k]);
    let mx = atomicLoad(&ga[k + 1u]);
    let my = atomicLoad(&ga[k + 2u]);
    var v = vec2f(0.0);
    var m = 0.0;
    if (mi > 0) {
        m = f32(mi) / u.mscale;
        v = vec2f(f32(mx), f32(my)) / (u.pscale * m);
        v += u.gravity * u.dt;
        v *= max(1.0 - u.damp * u.dt, 0.0);
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
    if (s < 3.0 * u.dx) {
        if (vs.x != 0.0 || vs.y != 0.0) {
            at -= vs * params.t * u.inv_dx;
            let f = floor(at);
            let t = at - f;
            let c = vec2i(f);
            let s00 = solid_at(c).x;
            let s10 = solid_at(c + vec2i(1, 0)).x;
            let s01 = solid_at(c + vec2i(0, 1)).x;
            let s11 = solid_at(c + vec2i(1, 1)).x;
            s = mix(mix(s00, s10, t.x), mix(s01, s11, t.x), t.y);
        }
        if (s < u.bc_band * u.dx) {
            let c = vec2i(round(at));
            let g = vec2f(
                solid_at(c + vec2i(1, 0)).x - solid_at(c - vec2i(1, 0)).x,
                solid_at(c + vec2i(0, 1)).x - solid_at(c - vec2i(0, 1)).x,
            );
            let len = length(g);
            if (s < -1.5 * u.dx || len < 1e-6) {
                v = vs;
            } else {
                let n = g / len;
                var vr = v - vs;
                let vn = dot(vr, n);
                if (vn < 0.0) {
                    vr -= vn * n;
                }
                // Friction as a rate: what slides along the solid loses this share per second.
                vr *= max(1.0 - u.friction * u.dt, 0.0);
                v = vr + vs;
            }
        }
    }
    let speed = length(v);
    if (speed > u.vmax) {
        v *= u.vmax / speed;
    }
    if (!(v.x == v.x && v.y == v.y)) {
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
    var p = parts[i];
    let xg = p.pos * u.inv_dx;
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
            let d = (vec2f(f32(gx), f32(gy)) - fx) * u.dx;
            let g = gf[cell(base, gx, gy)];
            v += wgt * g.xy;
            c0 += wgt * g.xy * d.x;
            c1 += wgt * g.xy * d.y;
            // A solid counts as fluid at rest, or particles would crowd against it.
            rho += wgt * (g.z + clamp(0.5 - g.w * u.inv_dx, 0.0, 1.0) * u.ghost * u.ppc);
            s += wgt * g.w;
            gs += wgt * g.w * d;
        }
    }
    let k = 4.0 * u.inv_dx * u.inv_dx;
    c0 *= k;
    c1 *= k;
    gs *= k;
    if (!(v.x == v.x && v.y == v.y)) {
        v = vec2f(0.0);
        c0 = vec2f(0.0);
        c1 = vec2f(0.0);
    }
    let step = v * u.dt;
    var x = p.pos + step;
    // Out of a solid, by the distance field the grid carries.
    let sn = s + dot(gs, step);
    let gl = length(gs);
    if (sn < 0.0 && gl > 1e-6) {
        let n = gs / gl;
        let vs = solid_at(vec2i(round(xg))).yz;
        // Out at a bounded speed (on top of the solid's own): fluid that a solid appears in
        // leaves over a few frames instead of bursting out.
        x += n * min(-sn, (u.push + length(vs)) * u.dt);
        // A particle the grid let into a solid must not keep heading inwards: a node just
        // outside the solid is free, and a lone particle would fall on it for ever.
        let vn = dot(v - vs, n);
        if (vn < 0.0) {
            v -= vn * n;
        }
    }
    let lo = vec2f(1.0 * u.dx);
    let hi = (vec2f(u.grid) - vec2f(2.0)) * u.dx;
    x = clamp(x, lo, hi);
    p.pos = x;
    p.sd = sn;
    p.vel = v;
    p.c0 = c0;
    p.c1 = c1;
    // The density where the particle is going: this gather's, carried along by the divergence.
    p.rho = rho * (1.0 - u.dt * (c0.x + c1.y));
    parts[i] = p;
}
