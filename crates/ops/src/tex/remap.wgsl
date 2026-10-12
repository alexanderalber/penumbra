// tex.remap: each pixel of the output fetches `in` (in0) at the position the `map` (in1) holds in
// its red and green (0, 0 top left, 1, 1 bottom right). An identity map (red ramp left to right,
// green top to bottom) gives the input back.

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let m = textureSampleLevel(in1, pn_clamp, v.uv, 0.0);
    return pn_extend(in0, m.rg, params.extend);
}
