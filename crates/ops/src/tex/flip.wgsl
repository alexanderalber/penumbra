// tex.flip: mirror the input left to right and/or top to bottom; `flop` swaps the axes first
// (the output is the input's height wide and its width high).

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    var uv = v.uv;
    if (params.flop != 0u) {
        uv = uv.yx;
    }
    if (params.flip_x != 0u) {
        uv.x = 1.0 - uv.x;
    }
    if (params.flip_y != 0u) {
        uv.y = 1.0 - uv.y;
    }
    return textureSampleLevel(in0, pn_clamp, uv, 0.0);
}
