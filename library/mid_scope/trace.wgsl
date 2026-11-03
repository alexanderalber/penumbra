// The trace of mid_scope, one short segment per texel, for geo.tex_instance with a thin prism one
// unit long along y. Together the segments draw a Lissajous figure: a closed line where x and z
// are sine waves of different frequency, as an oscilloscope draws two signals against each
// other. The levels of the music shape it. The mids set the frequency across, the treble the
// frequency up and down, and the bass its size. A frequency between two whole numbers blends
// the two figures, so the line stays closed. Each beat kicks the phase on, which turns the figure.
// in0: the stage's lane picture (its `lane` output); the figure floats over the lane. Unconnected:
// a straight lane at x = 0.
// `what` place: the middle of each segment. aim: its direction. scale: `thickness` across, its
// length along y (0 for none). Render it 16 x 16.
// @param what choice(place|aim|scale) = place -- what this node writes
// @param low float = 0 0..1 -- the bass (energy_low of a chan.beat_grid, smoothed): the figure grows
// @param mid float = 0 0..1 -- the mids (energy_mid): the frequency across
// @param high float = 0 0..1 -- the treble (energy_high): the frequency up and down
// @param beat_index float = 0 0..1000000 -- beats counted (beat_index of a chan.beat_grid)
// @param beat float = 0 0..1 -- the beat's progress (beat of a chan.beat_grid)
// @param presence float = 1 0..1 -- 1 the whole figure, towards 0 the line shortens, 0 none
// @param segments int = 192 16..256 -- segments in the line
// @param at float = 6 4..12 -- metres ahead of the camera
// @param hover float = 2.4 1.8..3.4 -- metres above the floor of the figure's middle
// @param across float = 1.5 0.2..4 -- half the figure's width, metres
// @param tall float = 0.8 0.1..1.5 -- half its height, metres
// @param depth float = 0.15 0..2 -- how far the line bends towards and away from the camera, metres
// @param grow float = 0.35 0..1 -- how much the bass enlarges the figure
// @param kick float = 0.125 0..0.5 -- turns of phase each beat adds
// @param thickness float = 0.01 0.001..0.1 -- metres across the line; it is drawn as thick as the wireframe's line_width at any size

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;
const TAU: f32 = 6.2831853;

// A sine of a frequency between whole numbers: the blend of the two whole ones around it, so
// the curve closes after one turn of `u`.
fn wave(f: f32, u: f32, phase: f32) -> vec2f {
    let lo = floor(f);
    let t = f - lo;
    let m = t * t * (3.0 - 2.0 * t);
    let a = lo * u + phase;
    let b = (lo + 1.0) * u + phase;
    return vec2f(mix(sin(a), sin(b), m), mix(cos(a), cos(b), m));
}

fn point(u: f32, low: f32, mid: f32, high: f32, phase: f32) -> vec3f {
    let size = 1.0 + params.grow * low;
    let fx = 1.0 + 3.0 * mid;
    // Always at least one more turn up and down than across: equal ones would draw a plain ellipse.
    let fz = fx + 1.0 + 2.0 * high;
    let w = wave(fx, u, phase);
    let h = wave(fz, u, 0.0);
    return vec3f(params.across * size * w.x, params.depth * w.y, params.tall * size * h.x);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let cell = vec2u(v.pos.xy);
    let i = cell.y * 16u + cell.x;
    let n = u32(clamp(params.segments, 16, 256));
    let low = clamp(params.low, 0.0, 1.0);
    let mid = clamp(params.mid, 0.0, 1.0);
    let high = clamp(params.high, 0.0, 1.0);
    // The phase steps on at each beat, quickly at first, then settling.
    let g = 1.0 - clamp(params.beat, 0.0, 1.0);
    let turns = fract(params.beat_index * params.kick) + (1.0 - g * g * g) * params.kick;
    let phase = TAU * turns;
    let u0 = TAU * f32(i) / f32(n);
    let u1 = TAU * f32(i + 1u) / f32(n);
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let lane = select(0.0, textureSampleLevel(in0, pn_clamp, vec2f((params.at - LANE_NEAR) / (LANE_FAR - LANE_NEAR), 0.5), 0.0).r, lane0.b > 0.0);
    let centre = vec3f(lane, params.at, params.hover);
    let a = centre + point(u0, low, mid, high, phase);
    let b = centre + point(u1, low, mid, high, phase);
    // Presence: the line is drawn up to that share of its length.
    let shown = clamp(params.presence, 0.0, 1.0) * f32(n);
    let part = clamp(shown - f32(i), 0.0, 1.0);
    let d = (b - a) * part;
    var len = length(d);
    if i >= n || params.presence < 0.001 {
        len = 0.0;
    }
    if params.what == what_aim {
        return vec4f(select(vec3f(0.0, 1.0, 0.0), d / max(len, 1e-6), len > 1e-6), 1.0);
    }
    if params.what == what_scale {
        let t = select(0.0, params.thickness, len > 0.0);
        // A little longer than the gap between points, so the joints close.
        return vec4f(t, len + select(0.0, 0.5 * params.thickness, len > 0.0), t, 1.0);
    }
    return vec4f(a + 0.5 * d, 1.0);
}
