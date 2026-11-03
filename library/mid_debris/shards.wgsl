// The shards of a burst rock, one per texel, drawn by geo.tex_instance with a shard. They fly
// out from where the rock is now (the rock flies on while it is left out), slow down and shrink
// away within `last` beats.
// in0: the burst's state (burst.wgsl). in1: the rocks (rocks.wgsl).
// Out: x, y, z of the shard in rgb, its size in alpha (0 while no burst runs). sizes.wgsl turns
// that into the scale. Render it 4 x 4.
// @param last float = 4 0.5..16 -- beats the shards fly, one bar by default
// @param reach float = 1.4 0..5 -- metres the shards fly out
// @param size float = 0.35 0.05..1.5 -- the rocks' mean size (rocks.wgsl's size): the shards are a fraction of the rock

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let burst = textureLoad(in0, vec2i(0, 0), 0);
    let cell = vec2u(v.pos.xy);
    let k = cell.y * u32(pn.resolution.x) + cell.x;
    let index = u32(burst.r + 0.5);
    let t = burst.b / max(params.last, 0.1);
    if index == 0u || t >= 1.0 {
        return vec4f(0.0, -100.0, 0.0, 0.0);
    }
    let dims = textureDimensions(in1);
    let n = index - 1u;
    let rock = textureLoad(in1, vec2i(vec2u(n % dims.x, n / dims.x)), 0);
    let r = vec3f(pn_hash(vec3u(index, k, 13u))) / 4294967296.0;
    let dir = normalize(2.0 * r - 1.0 + vec3f(0.0, 0.0, 0.001));
    // Out fast, then slowing; shrinking towards the end.
    let out = 1.0 - (1.0 - t) * (1.0 - t);
    let p = rock.xyz + dir * params.reach * mix(0.5, 1.0, r.x) * out;
    let s = params.size * mix(0.2, 0.5, r.y) * (1.0 - t * t);
    return vec4f(p, s);
}
