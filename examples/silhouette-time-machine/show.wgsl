// Stripes that never stop. in0 is the picture from seen.wgsl (when the person was last at each
// pixel), in1 the mask.
//
// Every pixel counts stripes at the same steady rate. A visit from the person sets its count
// back. So the count on the wall is the time since the person left, and a line of equal count
// is an outline the person had a moment ago. These lines run along the person's path behind
// them. They lie far apart where the person moved fast and close together where they moved
// slowly.
//
// The wall also has plain upright stripes running across it (`across`). They fill the parts the
// person has not reached yet, and they give every stripe the same lean.
// @param speed float = 3.3 -10..10 -- stripes a second passing a point; the same everywhere
// @param across float = 8.4 -60..60 -- upright stripes across the picture; above 0 they run to the right, below 0 to the left
// @param round float = 15 1..60 -- stripes until the colours come round again
// @param gap float = 0.39 0..0.95 -- black share of each stripe; 0 gives plain colour bands
// @param saturation float = 1 0..1 -- 0 gives white stripes on black
// @param look palette -- colours of one round; empty is a rainbow
// @param threshold float = 0.5 0..1 -- mask value from which a pixel counts as the person

fn rainbow(h: f32) -> vec3f {
    let k = fract(h + vec3f(0.0, 2.0 / 3.0, 1.0 / 3.0)) * 6.0;
    let c = clamp(abs(k - 3.0) - 1.0, vec3f(0.0), vec3f(1.0));
    return mix(vec3f(1.0), c, params.saturation);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let last = textureSampleLevel(in0, pn_clamp, v.uv, 0.0);
    // Time since the visit, on one float, so the count stays exact in a long show.
    let since = pn_time.seconds - (last.r + last.g) * 16.0;
    let count = since * params.speed - v.uv.x * params.across;

    // The edge of a stripe is one pixel wide. At a tear the count jumps, and the limit keeps
    // that from smearing into a grey band.
    let edge = clamp(fwidth(count), 0.001, 0.25);
    let place = fract(count);
    let lit = smoothstep(0.0, edge, place) * (1.0 - smoothstep(1.0 - params.gap - edge, 1.0 - params.gap, place));

    let person = textureSampleLevel(in1, pn_clamp, v.uv, 0.0).r;
    let hue = -count / max(params.round, 1.0);
    var colour = rainbow(hue);
    if params.look.count > 0 {
        colour = pn_colormap(params.look, fract(hue)).rgb;
    }
    return vec4f(colour * lit * (1.0 - step(params.threshold, person)), 1.0);
}
