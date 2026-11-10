// The small state of the ripples, 3 x 1 texels in 32-bit floats, kept from frame to frame by a
// tex.feedback. It starts over with the simulation, so a restart begins with the sources at
// phase 0 and no drop pending.
//   texel 0: r the sources' phase at the start of this frame (turns, 0..1), g the beat input of
//            the last frame, b seconds the water has been calm, a the clock at the last frame
//   texel 1: r 1 when a beat starts this frame, g 1 when a drop falls on calm water this frame,
//            b and a where that drop falls (shares of the picture)
//   texel 2: r seconds since the last frame, g drops so far (beat and calm), b the phase the
//            sources move on in this frame, a 1
// step.wgsl reads all three.
//
// in0: this node's own last frame (the tex.feedback).
//
// @param now float = 0 0..100000 -- seconds since the start; bound to the component's clock
// @param beat float = 0 0..1 -- the beat input; a rise above 0.5 is a beat
// @param ripple float = 1 0..10 -- how much the water moves (the component's ripple output, a frame late)
// @param steps int = 8 1..64 -- steps per frame; bound to the same number as the step's iterations
// @param wave_speed float = 0.3 0.05..0.85 -- wave speed in texels per step, as the step has it
// @param wavelength float = 24 6..120 -- of the sources, in texels, as the step has it
// @param calm_drop bool = false -- a drop falls by itself when the water has been calm for `idle` seconds
// @param idle float = 4 0.1..60 -- seconds of calm water before a drop falls by itself, while `calm_drop` is on
// @param still float = 0.004 0..0.2 -- ripple below which the water counts as calm
// @param pause bool = false -- holds the water as it is

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let x = i32(v.pos.x);
    let s0 = textureLoad(in0, vec2i(0, 0), 0);
    let s2 = textureLoad(in0, vec2i(2, 0), 0);
    let paused = params.pause != 0u;

    let dt = clamp(params.now - s0.a, 0.0, 0.1);
    let phase = fract(s0.r + s2.b);
    let beat = select(0.0, 1.0, params.beat > 0.5 && s0.g <= 0.5 && !paused);
    var calm = 0.0;
    if params.ripple < params.still && !paused {
        calm = s0.b + dt;
    }
    var idle = 0.0;
    if params.calm_drop != 0u && calm >= params.idle {
        idle = 1.0;
        calm = 0.0;
    }
    let count = s2.g + max(beat, idle);
    let h = pn_hash(vec3u(u32(count), 77u, 5u));
    let at = 0.15 + 0.7 * vec2f(f32(h.x >> 8u), f32(h.y >> 8u)) / 16777216.0;
    // The step node runs `steps` passes (1 to 64, as its iterations), each turning the sources
    // by wave_speed / wavelength.
    let turns = clamp(params.wave_speed, 0.0, 0.85) / max(params.wavelength, 2.0);
    let passes = f32(clamp(params.steps, 1, 64));
    let moves = select(passes * turns, 0.0, paused);

    if x == 0 {
        return vec4f(phase, params.beat, calm, params.now);
    }
    if x == 1 {
        return vec4f(beat, idle, at);
    }
    return vec4f(dt, count, moves, 1.0);
}
