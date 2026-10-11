// tex.cross: a mix of `a` (in0) and `b` (in1), 0 is all a, 1 all b. `b` is stretched to the
// size of `a`. Also what tex.switch renders when it blends two inputs.

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let a = textureSampleLevel(in0, pn_clamp, v.uv, 0.0);
    let b = textureSampleLevel(in1, pn_clamp, v.uv, 0.0);
    return mix(a, b, clamp(params.cross, 0.0, 1.0));
}
