// The velocities step: curl noise at the new position pushes each particle, then the velocity
// is damped. in0: this frame's positions (life in a), in1: last frame's velocities. Out: rgb the
// velocity, a the size (rises and falls over the particle's life).
//
// Curl noise is the curl of a noise field: a flow without sources or sinks, so the particles
// swirl instead of bunching up.
// @param speed float = 0.6 0..2 -- how fast the whole simulation runs
// @param delta float = 0.016667 0..0.1 -- the time step per frame
// @param noise vec4 = [0.5, 0.2, 0.2, 0.2] -- strength, frequency, change over time, amount
// @param vel_damping float = 0.96 0..1 -- velocity kept from one frame to the next

fn mod289_3(x: vec3f) -> vec3f { return x - floor(x * (1.0 / 289.0)) * 289.0; }
fn mod289_4(x: vec4f) -> vec4f { return x - floor(x * (1.0 / 289.0)) * 289.0; }
fn permute(x: vec4f) -> vec4f { return mod289_4(((x * 34.0) + 1.0) * x); }
fn taylor_inv_sqrt(r: vec4f) -> vec4f { return 1.79284291400159 - 0.85373472095314 * r; }

// 3D simplex noise by Ian McEwan and Stefan Gustavson (Ashima Arts), MIT licence:
// https://github.com/ashima/webgl-noise
fn snoise(v: vec3f) -> f32 {
    let C = vec2f(1.0 / 6.0, 1.0 / 3.0);
    let D = vec4f(0.0, 0.5, 1.0, 2.0);
    var i = floor(v + dot(v, C.yyy));
    let x0 = v - i + dot(i, C.xxx);
    let g = step(x0.yzx, x0.xyz);
    let l = 1.0 - g;
    let i1 = min(g.xyz, l.zxy);
    let i2 = max(g.xyz, l.zxy);
    let x1 = x0 - i1 + C.xxx;
    let x2 = x0 - i2 + C.yyy;
    let x3 = x0 - D.yyy;
    i = mod289_3(i);
    let p = permute(permute(permute(
        i.z + vec4f(0.0, i1.z, i2.z, 1.0))
        + i.y + vec4f(0.0, i1.y, i2.y, 1.0))
        + i.x + vec4f(0.0, i1.x, i2.x, 1.0));
    let ns = 0.142857142857 * D.wyz - D.xzx;
    let j = p - 49.0 * floor(p * ns.z * ns.z);
    let x_ = floor(j * ns.z);
    let y_ = floor(j - 7.0 * x_);
    let x = x_ * ns.x + ns.yyyy;
    let y = y_ * ns.x + ns.yyyy;
    let h = 1.0 - abs(x) - abs(y);
    let b0 = vec4f(x.xy, y.xy);
    let b1 = vec4f(x.zw, y.zw);
    let s0 = floor(b0) * 2.0 + 1.0;
    let s1 = floor(b1) * 2.0 + 1.0;
    let sh = -step(h, vec4f(0.0));
    let a0 = b0.xzyw + s0.xzyw * sh.xxyy;
    let a1 = b1.xzyw + s1.xzyw * sh.zzww;
    var p0 = vec3f(a0.xy, h.x);
    var p1 = vec3f(a0.zw, h.y);
    var p2 = vec3f(a1.xy, h.z);
    var p3 = vec3f(a1.zw, h.w);
    let norm = taylor_inv_sqrt(vec4f(dot(p0, p0), dot(p1, p1), dot(p2, p2), dot(p3, p3)));
    p0 *= norm.x;
    p1 *= norm.y;
    p2 *= norm.z;
    p3 *= norm.w;
    var m = max(0.6 - vec4f(dot(x0, x0), dot(x1, x1), dot(x2, x2), dot(x3, x3)), vec4f(0.0));
    m = m * m;
    return 42.0 * dot(m * m, vec4f(dot(p0, x0), dot(p1, x1), dot(p2, x2), dot(p3, x3)));
}

fn snoise_vec3(x: vec3f) -> vec3f {
    let s = snoise(x);
    let s1 = snoise(vec3f(x.y - 19.1, x.z + 33.4, x.x + 47.2));
    let s2 = snoise(vec3f(x.z + 74.2, x.x - 124.5, x.y + 99.4));
    return vec3f(s, s1, s2);
}

fn curl_noise(p: vec3f) -> vec3f {
    let e = 0.1;
    let dx = vec3f(e, 0.0, 0.0);
    let dy = vec3f(0.0, e, 0.0);
    let dz = vec3f(0.0, 0.0, e);
    let p_x0 = snoise_vec3(p - dx);
    let p_x1 = snoise_vec3(p + dx);
    let p_y0 = snoise_vec3(p - dy);
    let p_y1 = snoise_vec3(p + dy);
    let p_z0 = snoise_vec3(p - dz);
    let p_z1 = snoise_vec3(p + dz);
    let x = p_y1.z - p_y0.z - p_z1.y + p_z0.y;
    let y = p_z1.x - p_z0.x - p_x1.z + p_x0.z;
    let z = p_x1.y - p_x0.y - p_y1.x + p_y0.x;
    let c = vec3f(x, y, z) / (2.0 * e);
    // A zero vector has no direction: no push.
    return select(vec3f(0.0), normalize(c), dot(c, c) > 1e-20);
}

fn parabola(x: f32, k: f32) -> f32 {
    return pow(max(4.0 * x * (1.0 - x), 0.0), k);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let at = vec2i(v.pos.xy);
    let p = textureLoad(in0, at, 0);
    var vel = textureLoad(in1, at, 0).xyz;
    // The field is sampled with y up, z towards the camera; positions here are z up.
    let q = vec3f(p.x, p.z, -p.y);
    let f = params.noise;
    let n = params.delta * f.x * curl_noise(q * f.y + pn_time.seconds * f.z * params.speed);
    vel += params.speed * vec3f(n.x, -n.z, n.y) * f.w;
    vel *= params.vel_damping;
    return vec4f(vel, parabola(1.0 - p.w, 0.4));
}
