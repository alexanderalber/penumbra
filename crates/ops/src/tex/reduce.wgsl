// chan.from_tex: one step of reducing a picture to `cells_x` x `cells_y` areas. The output has a
// whole number of texels per area, and each texel covers an exact range of whole source texels
// inside its area: their sum, their minimum or their maximum. Every source texel is counted once.
// The first step reads the picture itself. There a pixel may become its luminance (in red), and
// for a count 1 where it is above `threshold`, else 0.

// Stands for "no texel yet" in a minimum or maximum; within the range of 32-bit floats.
const NONE: f32 = 3.0e38;

fn prepare(c: vec4f) -> vec4f {
    var v = c;
    if params.first != 0 {
        if params.luminance != 0 {
            v = vec4f(pn_luma(c.rgb), 0.0, 0.0, 0.0);
        }
        if params.above != 0 {
            v = select(vec4f(0.0), vec4f(1.0), v > vec4f(params.threshold));
        }
    }
    return v;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let src = vec2i(textureDimensions(in0));
    let cells = max(vec2i(params.cells_x, params.cells_y), vec2i(1));
    let per = max(vec2i(pn.resolution) / cells, vec2i(1));
    let o = vec2i(floor(v.pos.xy));
    let cell = min(o / per, cells - vec2i(1));
    let part = o - cell * per;
    // The area's texels in the source, at least one.
    let a = min(cell * src / cells, src - vec2i(1));
    let b = max((cell + vec2i(1)) * src / cells, a + vec2i(1));
    let len = b - a;
    let lo = a + part * len / per;
    let hi = a + (part + vec2i(1)) * len / per;
    var acc = vec4f(0.0);
    if params.mode == 1 {
        acc = vec4f(NONE);
    } else if params.mode == 2 {
        acc = vec4f(-NONE);
    }
    for (var y = lo.y; y < hi.y; y++) {
        for (var x = lo.x; x < hi.x; x++) {
            let c = prepare(textureLoad(in0, vec2i(x, y), 0));
            if params.mode == 1 {
                acc = min(acc, c);
            } else if params.mode == 2 {
                acc = max(acc, c);
            } else {
                acc += c;
            }
        }
    }
    return acc;
}
