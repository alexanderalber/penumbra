// Each agent's deposit, for tex.scatter's `values`: three weights by its heading that add up to
// 1, so r + g + b of the deposit counts agents and the colour says where they head. Alpha 1: the
// deposit's alpha is the exact number of agents per cell.
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let s = textureLoad(in0, vec2i(v.pos.xy), 0);
    let a = s.b * 6.2831853;
    let w = (1.0 + cos(a - vec3f(0.0, 2.0943951, 4.1887902))) / 3.0;
    return vec4f(w, 1.0);
}
