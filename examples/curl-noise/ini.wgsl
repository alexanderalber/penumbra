// Where particles start, one per texel: a random point of a round cloud around the centre (a
// normal distribution per axis, about as wide as the swarm stays later), so the first frames
// already look like the running flow; and a random life for each. A spent particle starts
// again at its point here.
// rgb: the position, a: life 0..1.
// @param spread float = 1.6 0.1..5 -- the cloud's standard deviation per axis
fn unit(x: u32) -> f32 {
    return (f32(x >> 8u) + 0.5) / 16777216.0;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let at = vec2u(v.pos.xy);
    let a = pn_hash(vec3u(at, 11u));
    let b = pn_hash(vec3u(at, 13u));
    // Box-Muller: two pairs of uniform numbers give four normal ones; three are used.
    let r0 = sqrt(-2.0 * log(unit(a.x)));
    let r1 = sqrt(-2.0 * log(unit(a.z)));
    let t0 = 6.2831853 * unit(a.y);
    let t1 = 6.2831853 * unit(b.x);
    let n = vec3f(r0 * cos(t0), r0 * sin(t0), r1 * cos(t1));
    let h = pn_hash(vec3u(at, 7u));
    let life = f32(h.x >> 8u) / 16777216.0;
    return vec4f(n * params.spread, life);
}
