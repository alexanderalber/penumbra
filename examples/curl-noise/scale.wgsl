// The sticks' scale: their length is the size in in0's alpha times a pulse.
// @param pulse float = 1 0..2 -- bound to /pulse
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let size = textureLoad(in0, vec2i(v.pos.xy), 0).a;
    return vec4f(1.0, size * params.pulse, 1.0, 1.0);
}
