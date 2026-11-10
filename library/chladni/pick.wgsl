// Which figure sounds, kept as one pixel from frame to frame (run in a tex.feedback loop,
// format 32f). Figures are numbered by pitch: 0 is the plate's lowest tone, 19 its highest.
// in0: the state of the frame before. in1: the membrane's state, for its size (the pitch is
// counted in membrane texels).
//
// State: r = the figure in effect, g = the clock time when the figure last changed (for the
// toss at a new figure), b = the membrane's drive phase at the start of this frame, a = the
// drive's angle per membrane step. The phase runs on in whole turns, so it stays exact however
// long the show runs.
//
// @param source choice(tone|figure) = tone -- tone: the figure follows the channel `note` of the input tone; figure: the param figure
// @param note float = 54 0..127 -- follows the input tone: a note number (60 is middle C)
// @param figure float = 6 0..19 -- the figure when source is figure
// @param lowest float = 48 0..127 -- the note of figure 0 (48 is C3); each semitone up is the next figure
// @param morph bool = false -- off: the nearest whole figure; on: a fraction of a semitone blends two neighbours
// @param detune float = 1 0.5..2 -- shaped plate: multiplies the membrane's pitch; sweep it to find the shape's own tones
// @param now float = 0 -- follows the component's clock (seconds since the start)
// @param msteps float = 1 -- follows the component's membrane_steps: membrane steps per frame

const TAU = 6.2831853;
const SPEED = 0.7;
const SIZE = 0.9;

// The pitch of figure k of a square plate: sqrt(m^2 + n^2) of the figure table of `field`.
fn sq_pitch(k: i32) -> f32 {
    var t = array<vec2f, 20>(
        vec2f(1.0, 0.0), vec2f(2.0, 0.0), vec2f(2.0, 1.0), vec2f(2.0, 1.0),
        vec2f(3.0, 0.0), vec2f(3.0, 1.0), vec2f(3.0, 2.0), vec2f(4.0, 0.0),
        vec2f(4.0, 1.0), vec2f(4.0, 2.0), vec2f(5.0, 0.0), vec2f(4.0, 3.0),
        vec2f(5.0, 2.0), vec2f(5.0, 3.0), vec2f(6.0, 1.0), vec2f(6.0, 3.0),
        vec2f(7.0, 2.0), vec2f(7.0, 4.0), vec2f(8.0, 3.0), vec2f(9.0, 4.0));
    let w = t[clamp(k, 0, 19)];
    return sqrt(w.x * w.x + w.y * w.y);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let prev = textureLoad(in0, vec2i(0, 0), 0);
    var f = params.figure;
    if params.source == source_tone {
        f = params.note - params.lowest;
    }
    if !(params.morph != 0u) {
        f = round(f);
    }
    f = clamp(f, 0.0, 19.0);
    // The first frame (no drive angle yet) is no change of figure.
    let first = prev.a <= 0.0;
    var changed_at = select(prev.g, -1000.0, first);
    if !first && round(f) != round(prev.r) {
        changed_at = params.now;
    }
    let k0 = i32(floor(f));
    let pitch = mix(sq_pitch(k0), sq_pitch(k0 + 1), f - floor(f));
    let side = SIZE * f32(textureDimensions(in1).y);
    let omega = SPEED * 3.14159265 * pitch / side * params.detune;
    var phase = 0.0;
    if !first {
        phase = prev.b + prev.a * max(params.msteps, 1.0);
        phase = phase - TAU * floor(phase / TAU);
    }
    return vec4f(f, changed_at, phase, omega);
}
