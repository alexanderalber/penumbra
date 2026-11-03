// The scale per solid of one kind, from the places (formation.wgsl, `what` place, in in0). Each
// solid's kind comes from a hash of the solid and `seed`; solids of another kind get scale 0.
// Render it at the size of in0.
// @param kind int = 0 0..2 -- the kind this node scales: 0 octahedron, 1 torus, 2 icosahedron
// @param seed int = 0 0..1000 -- another seed, other shapes in the ring
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let cell = vec2u(v.pos.xy);
    let p = textureLoad(in0, vec2i(cell), 0);
    let h = pn_hash(vec3u(cell.x, 7u, u32(max(params.seed, 0)))).x;
    // Solid k takes kind (k + offset) mod 3 for most seeds, so every ring mixes the kinds; the
    // hash swaps one solid's kind now and then.
    let offset = pn_hash(vec3u(u32(max(params.seed, 0)), 3u, 5u)).x % 3u;
    var kind = (cell.x + offset) % 3u;
    if h % 4u == 0u {
        kind = (kind + 1u + (h >> 8u) % 2u) % 3u;
    }
    let s = select(0.0, p.a, kind == u32(clamp(params.kind, 0, 2)));
    return vec4f(s, s, s, 1.0);
}
