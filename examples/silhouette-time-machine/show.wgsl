// The final picture. in0 is the time machine (the pattern's place in the round of stripes, as
// cosine and sine in red and green), in1 the picture from seen.wgsl (blue 1 where the person has
// been), in2 the mask.
//
// On top of the time machine's stripes come stripes around the person: each pixel looks along
// its row for the nearest pixel of the person, and that distance adds to the place in the round.
// So near the person the stripes copy their outline, and they run towards them as the pattern
// moves. The picture stays black where the person has never been, and on the person.
// @param local float = 10 0..40 -- stripes around the person, between them and the far edge; 0: the time machine's stripes alone
// @param bunch float = 0.5 0.2..1 -- how much those stripes crowd near the person; 1: evenly spaced
// @param gap float = 0.4 0..0.9 -- share of each stripe that is black
// @param round int = 24 1..60 -- stripes until the colours come round again; the same as in pattern.wgsl
// @param look palette -- colours to blend through instead of the rainbow, once per round
// @param saturation float = 1 0..1 -- of the rainbow
// @param threshold float = 0.5 0..1 -- mask value from which a pixel counts as the person
// @param step int = 2 1..8 -- pixels between the looks along a row; higher is faster and coarser
// @group above the head
// @param above float = 0.3 0..1 -- how far rows above the person look down for them, in image heights

fn is_person(p: vec2i, size: vec2i) -> bool {
    if p.x < 0 || p.x >= size.x || p.y < 0 || p.y >= size.y {
        return false;
    }
    return textureLoad(in2, p, 0).r >= params.threshold;
}

// Distance in pixels along row y from x to the nearest pixel of the person, or -1 for none.
fn along_row(x: i32, y: i32, size: vec2i) -> f32 {
    let step = max(params.step, 1);
    let far = max(x, size.x - 1 - x);
    for (var d = step; d <= far; d += step) {
        if is_person(vec2i(x - d, y), size) || is_person(vec2i(x + d, y), size) {
            return f32(d);
        }
    }
    return -1.0;
}

fn rainbow(h: f32) -> vec3f {
    let k = fract(h + vec3f(0.0, 2.0 / 3.0, 1.0 / 3.0)) * 6.0;
    let c = clamp(abs(k - 3.0) - 1.0, vec3f(0.0), vec3f(1.0));
    return mix(vec3f(1.0), c, params.saturation);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let size = vec2i(textureDimensions(in2));
    let p = vec2i(v.uv * vec2f(size));
    let seen = textureSampleLevel(in1, pn_clamp, v.uv, 0.0);
    if seen.b < 0.5 || is_person(p, size) {
        return vec4f(0.0, 0.0, 0.0, 1.0);
    }

    // The time machine's place in the round, in stripes.
    let round = f32(max(params.round, 1));
    let cs = textureSampleLevel(in0, pn_clamp, v.uv, 0.0).rg * 2.0 - 1.0;
    var phase = atan2(cs.y, cs.x) / 6.2831853 * round;

    if params.local > 0.0 {
        var d = along_row(p.x, p.y, size);
        if d < 0.0 {
            // No part of the person in this row (above the head): the nearest row below that
            // has one, with the height in between counted in.
            let rows = i32(params.above * f32(size.y));
            let jump = max(params.step, 1) * 4;
            for (var dy = jump; dy <= rows; dy += jump) {
                let below = along_row(p.x, p.y + dy, size);
                if below >= 0.0 {
                    d = length(vec2f(below, f32(dy)));
                    break;
                }
            }
        }
        if d < 0.0 {
            d = f32(size.x);
        }
        // 0 at the person, 1 at the width of the picture; the power crowds the stripes near
        // them. Subtracted, so the stripes run towards the person while the pattern moves on.
        phase -= pow(d / f32(size.x), params.bunch) * params.local;
    }

    // Whole stripes, so the round closes without a seam.
    phase -= round * floor(phase / round);
    if fract(phase) >= 1.0 - params.gap {
        return vec4f(0.0, 0.0, 0.0, 1.0);
    }
    let h = floor(phase) / round;
    if params.look.count > 0 {
        return vec4f(pn_colormap(params.look, h).rgb, 1.0);
    }
    return vec4f(rainbow(h), 1.0);
}
