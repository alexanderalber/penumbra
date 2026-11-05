// One model's scale from the plants' scale picture (reeds.wgsl, `what` scale; its alpha is the
// kind, plus 10 for a clump that carries a mast): the clump's size where it is this model, else 0.
// `kind` 0 to 4 is a plant, 14 the mast. Set the node's resolution to `input`.
// @param kind float = 0 0..14 -- which model this is

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let c = textureLoad(in0, vec2i(v.pos.xy), 0);
    let mine = abs(c.a - params.kind) < 0.5 || abs(c.a - params.kind - 10.0) < 0.5;
    return vec4f(select(vec3f(0.0), c.rgb, mine), 1.0);
}
