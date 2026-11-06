// Dark scanlines over a picture, like an old monitor. The lines roll slowly down the picture as
// the world travels, and grow darker for a moment on every onset, so they flicker with the
// music. in0: the picture.
// @param pitch float = 4 1..64 -- pixels from one line to the next
// @param strength float = 0.45 0..1 -- how dark a line is at its middle: 0 none, 1 black
// @param thickness float = 0.5 0.05..0.95 -- how much of the pitch a line covers
// @param roll float = 0.25 -4..4 -- lines the pattern moves down per beat travelled; negative moves it up
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param flash float = 0 0..1 -- a flicker, bind it to a decaying onset of a chan.beat_grid
// @param flicker float = 0.35 0..1 -- how much darker the lines and the picture get on a flash
// @param presence float = 1 0..1 -- how much of the effect is there: 1 all of it, 0 the picture unchanged
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let c = textureLoad(in0, vec2i(v.pos.xy), 0);
    let pitch = max(params.pitch, 1.0);
    let flash = clamp(params.flash, 0.0, 1.0) * clamp(params.flicker, 0.0, 1.0);
    // Where this pixel lies within its line, 0 to 1, the pattern moved by the roll.
    let f = fract(v.pos.y / pitch - params.beats * params.roll);
    // Distance from the line's middle in pitches, and a soft edge of about a pixel.
    let d = abs(f - 0.5);
    let half_width = 0.5 * clamp(params.thickness, 0.05, 0.95);
    let edge = min(0.5 / pitch, 0.25);
    let line = 1.0 - smoothstep(half_width - edge, half_width + edge, d);
    let strength = clamp(params.strength + 0.5 * flash, 0.0, 1.0);
    let shade = (1.0 - strength * line) * (1.0 - 0.3 * flash);
    let presence = clamp(params.presence, 0.0, 1.0);
    return vec4f(c.rgb * mix(1.0, shade, presence), c.a);
}
