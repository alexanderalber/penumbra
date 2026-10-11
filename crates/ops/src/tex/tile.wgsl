// tex.tile: the input repeated `repeat` times across and down at the input's resolution, every
// other copy mirrored if asked; `offset` shifts the pattern (in tiles).

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let uv = v.uv * params.repeat + params.offset;
    if (params.mirror != 0u) {
        return textureSampleLevel(in0, pn_mirror, uv, 0.0);
    }
    return textureSampleLevel(in0, pn_repeat, uv, 0.0);
}
