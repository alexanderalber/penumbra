// How wide the written title is: render it 1 x 1. in0 and in1: two pictures of one line of
// text each (tex.text). Out: red and green the left and the right edge of what is written in
// in0, blue and alpha the same for in1, as shares of the picture's width. A picture with
// nothing written gives 0.5 and 0.5.

fn extent(n: u32) -> vec2f {
    var size: vec2u;
    if n == 0u {
        size = textureDimensions(in0);
    } else {
        size = textureDimensions(in1);
    }
    let w = max(size.x, 1u);
    let h = max(size.y, 1u);
    // Every second column, eight rows across the middle half of the picture.
    var lo = w;
    var hi = 0u;
    for (var x = 0u; x < w; x += 2u) {
        var lit = false;
        for (var r = 0u; r < 8u; r++) {
            let y = h / 4u + (h / 2u) * r / 8u;
            var a: f32;
            if n == 0u {
                a = textureLoad(in0, vec2u(x, y), 0).a;
            } else {
                a = textureLoad(in1, vec2u(x, y), 0).a;
            }
            lit = lit || a > 0.25;
        }
        if lit {
            lo = min(lo, x);
            hi = max(hi, x + 2u);
        }
    }
    if hi <= lo {
        return vec2f(0.5);
    }
    return vec2f(f32(lo), f32(hi)) / f32(w);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    return vec4f(extent(0u), extent(1u));
}
