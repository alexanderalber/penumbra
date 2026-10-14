// Where particles start, one per texel: smooth noise per channel (two octaves) for the
// position, a random life for each.
// rgb: a position in about -1..1, a: life 0..1.
// @param period float = 1 0.1..8 -- noise features across the texture
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = vec3f(v.uv * params.period, 0.0);
    var rgb = vec3f(0.0);
    for (var c = 0u; c < 3u; c++) {
        let n = pn_noise(p, c) + 0.5 * pn_noise(p * 2.0, c + 3u);
        rgb[c] = n / 1.5;
    }
    let h = pn_hash(vec3u(vec2u(v.pos.xy), 7u));
    let life = f32(h.x >> 8u) / 16777216.0;
    return vec4f(rgb, life);
}
