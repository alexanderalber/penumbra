// A simple moving pattern for the time machine: stripes that scroll across the picture.
// Nothing here knows about the person; the time machine bends the stripes.
//
// The picture holds no colours yet, only where each pixel lies in the round of stripes: red and
// green are the cosine and sine of that position. So show.wgsl can still add stripes of its own
// before it picks the colours, and a round that starts again shows no seam.
// @param shape choice(wavy|straight|rings) = wavy -- wavy: upright stripes that sway; straight: upright stripes; rings: circles around the middle
// @param stripes float = 20 1..80 -- stripes across the picture
// @param speed float = 1.5 -10..10 -- stripes a second passing a point; below 0 the other way
// @param round int = 24 1..60 -- stripes until the colours come round again; the same as in show.wgsl
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let aspect = pn.resolution.x / pn.resolution.y;
    let p = vec2f(v.uv.x * aspect, v.uv.y);
    // 0 to 1 across the picture.
    var across = v.uv.x;
    if params.shape == shape_wavy {
        across += 0.03 * sin(p.y * 9.0 + pn_time.seconds * 0.7);
    } else if params.shape == shape_rings {
        across = length(p - vec2f(aspect * 0.5, 0.5)) / aspect;
    }
    let phase = across * params.stripes - pn_time.seconds * params.speed;
    let angle = 6.2831853 * phase / f32(max(params.round, 1));
    return vec4f(0.5 + 0.5 * cos(angle), 0.5 + 0.5 * sin(angle), 0.0, 1.0);
}
