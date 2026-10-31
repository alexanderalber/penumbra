// The map for the time machine: how long ago the person was last at each pixel. in0 is the
// picture from seen.wgsl (red and green the time in 16-second steps, blue 1 where the person
// has been). Black is now; white is `span` seconds ago or longer, and wherever the person has
// never been.
// @param span float = 20 0.1..120 -- seconds that white stands for; the time machine's length in seconds
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let seen = textureSampleLevel(in0, pn_clamp, v.uv, 0.0);
    var back = 1.0;
    if seen.b > 0.5 {
        let since = pn_time.seconds - (seen.r + seen.g) * 16.0;
        back = clamp(since / params.span, 0.0, 1.0);
    }
    return vec4f(vec3f(back), 1.0);
}
