// A plate of any outline: a vibrating membrane inside the white of in1, driven at one point with
// the pitch of the figure `pick` chooses (in2). It runs the component's membrane_steps per
// frame in a tex.feedback loop, format 32f. After a second or so the motion settles into a
// standing pattern.
//
// The pattern is measured like a lock-in amplifier: a running mean of height times cos and times
// sin of the drive. That gives each point's swing and its timing. `field` keeps the part of the
// swing in step with the timing most of the plate shares. Its zeros are whole, unbroken nodal
// lines. (The plain mean square has no exact zeros where the plate is damped, so the sand would
// gather in short dashes instead of lines.)
//
// This is the wave equation of a membrane, not the bending of a metal plate: the figures are of
// the right kind, not the exact ones of a metal plate of that outline.
//
// State: r = height, g = velocity (height change per step), b = mean of height times cos,
// a = mean of height times sin. Off the plate: g = 30000, a marker no real velocity reaches.
// Edges are free (the slope across the edge is 0), as for a plate held in the middle.
// The drive's phase and its angle per step come from `pick` (b, a), so the drive runs on without
// a jump however long the show runs.
// With a square or circular plate the state passes through unchanged.
//
// @param plate choice(square|circle|shape) = square -- follows the component's plate
// @param damping float = 0.002 0..0.05 -- share of the motion lost per step; lower: sharper tones, slower to settle
// @param drive float = 1 0..10 -- strength of the driving point
// @param drive_at vec2 = [0.5, 0.5] -- where the plate is driven, in picture sizes
// @param drive_radius float = 3 1..20 -- radius of the driving point in membrane texels
// @param average float = 0.002 0.0002..0.05 -- how fast the measured pattern follows, per step

const OFF = 30000.0;
const SPEED = 0.7;

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = vec2i(v.pos.xy);
    let n = vec2i(textureDimensions(in0));
    let here = textureLoad(in0, p, 0);
    if params.plate != plate_shape {
        return here;
    }
    let uv = (vec2f(p) + 0.5) / vec2f(n);
    let inside = pn_luma(textureSampleLevel(in1, pn_clamp, uv, 0.0).rgb) > 0.5;
    if !inside {
        return vec4f(0.0, OFF, 0.0, 0.0);
    }
    // A texel that just joined the plate starts at rest.
    var u = here.r;
    var vel0 = here.g;
    var lock = here.ba;
    if here.g > 0.5 * OFF {
        u = 0.0;
        vel0 = 0.0;
        lock = vec2f(0.0);
    }
    // Free edges: a neighbour off the plate (or off the picture) counts as this texel.
    var sides = 0.0;
    var corners = 0.0;
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            if x != 0 || y != 0 {
                let q = p + vec2i(x, y);
                var h = u;
                if all(q >= vec2i(0)) && all(q < n) {
                    let t = textureLoad(in0, q, 0);
                    h = select(t.r, u, t.g > 0.5 * OFF);
                }
                if x == 0 || y == 0 {
                    sides += h;
                } else {
                    corners += h;
                }
            }
        }
    }
    // Written so that a flat membrane gives exactly 0 in 32-bit floats.
    let lap = (2.0 / 3.0) * (sides - 4.0 * u) + (1.0 / 6.0) * (corners - 4.0 * u);

    let pk = textureLoad(in2, vec2i(0, 0), 0);
    let ph = pk.b + pk.a * f32(pn.iteration);
    let d = distance(vec2f(p) + 0.5, params.drive_at * vec2f(n));
    let r = params.drive_radius;
    let force = params.drive * 0.1 * exp(-d * d / (r * r)) * sin(ph);

    let vel = vel0 * (1.0 - params.damping) + SPEED * SPEED * lap + force;
    let un = u + vel;
    lock = lock + params.average * (2.0 * un * vec2f(cos(ph), sin(ph)) - lock);
    return vec4f(un, vel, lock);
}
