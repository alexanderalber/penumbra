// Where each copy points while it tumbles: a direction per texel for geo.tex_instance's `aim`,
// turning about z and nodding up and down, at its own rate from a hash of the texel. Render it
// at the size of the place picture.
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param spin float = 0.12 0..2 -- turns per beat, at most
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let cell = vec2u(v.pos.xy);
    let r = vec3f(pn_hash(vec3u(cell, 9u))) / 4294967296.0;
    let w = params.spin * mix(0.3, 1.0, r.x) * select(-1.0, 1.0, r.y < 0.5);
    // Only the fraction of the turns matters; taking it before the angle keeps it exact.
    let a = 6.2831853 * (fract(params.beats * w) + r.z);
    let b = 1.1 * sin(6.2831853 * (fract(params.beats * w * 0.71) + r.y));
    return vec4f(cos(a) * cos(b), sin(a) * cos(b), sin(b), 1.0);
}
