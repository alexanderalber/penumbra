// Scale or light per fog bank, from the banks' places (banks.wgsl in in0). `scale`: width and
// height per bank, a little different for each. `light`: the bank's share of presence times
// the bass, fading out as the bank comes within `fade` metres of the camera, so the camera passes
// through without a flash, and fading into the distance as the stage's lines do (a bank is too
// faint for the stage's own fade, which goes by the render's data pass). A bank whose share is gone is scaled to nothing. Render it at the
// size of in0.
// @param what choice(scale|light) = scale -- what this node writes
// @param across float = 6 0.5..15 -- a bank's width, metres
// @param tall float = 1.6 0.2..4 -- a bank's height, metres
// @param bass float = 0 0..1 -- the bass (energy_low of a chan.beat_grid): the banks glow with it
// @param brightness float = 4 0..4 -- the banks' light at full bass
// @param quiet float = 0.25 0..1 -- the share of that light left without bass
// @param fade float = 3 0.5..8 -- metres before the camera where a bank has faded out
// @param start float = 4 0..50 -- metres where the distance fade begins (the stage's `start`)
// @param end float = 10 0..100 -- metres where nothing is left (the stage's `end`)
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let cell = vec2u(v.pos.xy);
    let p = textureLoad(in0, vec2i(cell), 0);
    let r = vec3f(pn_hash(vec3u(cell, 21u))) / 4294967296.0;
    if params.what == what_scale {
        let on = select(0.0, 1.0, p.a > 0.01);
        // Mirrored at random, so the banks do not all show the same noise the same way round.
        let flip = select(-1.0, 1.0, r.z < 0.5);
        return vec4f(flip * params.across * mix(0.7, 1.3, r.x) * on, on, params.tall * mix(0.7, 1.3, r.y) * on, 1.0);
    }
    let near = smoothstep(0.3, params.fade, p.y) * (1.0 - smoothstep(params.start, params.end, p.y));
    return vec4f(vec3f(p.a * near * params.brightness * mix(params.quiet, 1.0, clamp(params.bass, 0.0, 1.0))), 1.0);
}
