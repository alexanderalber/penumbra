// The scale per copy from a place picture in in0 (rocks.wgsl or shards.wgsl): the size in alpha,
// stretched a little per axis by a hash of the texel, so copies of one model differ. Render it at
// the size of in0.
// @param stretch float = 0.35 0..1 -- how unevenly the axes are stretched
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let cell = vec2u(v.pos.xy);
    let p = textureLoad(in0, vec2i(cell), 0);
    let r = vec3f(pn_hash(vec3u(cell, 5u))) / 4294967296.0;
    return vec4f(p.a * (1.0 + params.stretch * (2.0 * r - 1.0)), 1.0);
}
