// Thin lines where two faces meet and round the objects. in0: a labels picture (scan_labels'
// `labels`: red the face, green the object, k / 255).
// @param line float = 2 1..8 -- line width, pixels
// @param color color = [1, 0.82, 0.55, 1]
// @param objects bool = true -- also draw round the objects
// @param glow float = 0.35 0..1 -- a soft halo either side of the line

// A label of /clean: its red is the face, its green the object, k / 255.
fn label(t: texture_2d<f32>, p: vec2i, channel: i32) -> i32 {
    let size = vec2i(textureDimensions(t));
    let c = textureLoad(t, clamp(p, vec2i(0), size - 1), 0);
    return i32(round(select(c.r, c.g, channel == 1) * 255.0));
}

// Faces by their number, objects above 256, nothing 0.
fn id(p: vec2i) -> i32 {
    let o = label(in0, p, 1);
    if (params.objects == 1u && o > 0) {
        return 256 + o;
    }
    if (o > 0) {
        return 0;
    }
    return label(in0, p, 0);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = vec2i(v.pos.xy);
    let here = id(p);
    if (here == 0) {
        return vec4f(0.0);
    }
    // Distance to the nearest pixel of another face, searched out to the halo.
    let reach = i32(ceil(params.line * 2.0)) + 2;
    var nearest = 1e9;
    for (var dy = -reach; dy <= reach; dy++) {
        for (var dx = -reach; dx <= reach; dx++) {
            let d = length(vec2f(f32(dx), f32(dy)));
            if (d >= nearest || d > f32(reach)) {
                continue;
            }
            let other = id(p + vec2i(dx, dy));
            if (other != here) {
                nearest = d;
            }
        }
    }
    let half = params.line * 0.5;
    let core = 1.0 - smoothstep(half - 0.5, half + 0.5, nearest);
    let halo = params.glow * exp(-max(nearest - half, 0.0) / max(params.line, 1.0));
    let a = clamp(core + select(0.0, halo, nearest < 1e8), 0.0, 1.0);
    return vec4f(params.color.rgb * a, a);
}
