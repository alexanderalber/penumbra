// A picture kept only on the objects: in0 the picture (the height), in1 a labels picture
// (scan_labels' `labels`, green the object). Elsewhere transparent, so the contour lines after it
// follow the objects alone.

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = vec2i(v.pos.xy);
    let o = textureLoad(in1, p, 0).g;
    if (o < 0.5 / 255.0) {
        return vec4f(0.0);
    }
    return textureLoad(in0, p, 0);
}
