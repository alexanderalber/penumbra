// White on the faces, transparent on the objects and where nothing was measured.
// in0: a labels picture (scan_labels' `labels`).

// A label of /clean: its red is the face, its green the object, k / 255.
fn label(t: texture_2d<f32>, p: vec2i, channel: i32) -> i32 {
    let size = vec2i(textureDimensions(t));
    let c = textureLoad(t, clamp(p, vec2i(0), size - 1), 0);
    return i32(round(select(c.r, c.g, channel == 1) * 255.0));
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = vec2i(v.pos.xy);
    let on = label(in0, p, 0) > 0 && label(in0, p, 1) == 0;
    return select(vec4f(0.0), vec4f(1.0), on);
}
