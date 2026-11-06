// The streaks of mid_particles, one per texel, drawn by geo.tex_instance with a thin prism.
// They stream towards the camera against the flight, `speed` times as fast as the floor, and
// pass it on all sides. Each pass of a streak takes a new place across, from a hash.
// Streaks keep out of two boxes: a tube around the camera's line of sight (every preset's
// height) and the space low in the middle where the glider flies.
// in0: the stage's lane picture (its `lane` output); only its blue, `step`, is read. Unconnected:
// `step` metres per beat.
// Out: x, y, z of the streak's centre in rgb, its length in alpha (0 where none flies). A second
// node (sizes.wgsl) turns that into the streaks' scale. Render it 64 x 16.
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param bpm float = 120 40..240 -- the pace of `beats` (travel_bpm of a chan.beat_grid): faster music, longer streaks
// @param onset float = 0 0..1 -- recent onsets, smoothed: more streaks fly while the music is busy
// @param presence float = 1 0..1 -- 1 the full stream, towards 0 it thins out, 0 none
// @param density float = 0.45 0..1 -- share of streaks flying in quiet music; onsets raise it to all
// @param speed float = 6 1..20 -- how much faster than the floor the streaks fly
// @param length float = 0.8 0.05..4 -- a streak's length at 120 BPM, metres
// @param spread float = 5 1..12 -- half the width of the stream, metres
// @param near float = -1.5 -8..0 -- where a streak leaves, behind the camera
// @param far float = 14 4..56 -- where it appears
// @param step float = 0.5 0.1..2 -- metres per beat while no lane picture is wired; with one, the stage's step

// The ceiling's height and a margin to the floor and the ceiling.
const CEILING: f32 = 4.0;
const MARGIN: f32 = 0.15;

fn rand(i: u32, lap: u32, k: u32) -> vec3f {
    return vec3f(pn_hash(vec3u(i, lap, k))) / 4294967296.0;
}

// Whether a place across is free: outside the tube around the camera's line of sight and outside
// the glider's space, low in the middle.
fn free(x: f32, z: f32) -> bool {
    let camera = abs(x) < 0.7 && z > 1.0 && z < 3.2;
    let glider = abs(x) < 2.3 && z < 1.9;
    return !(camera || glider);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let step = select(params.step, lane0.b, lane0.b > 0.0);
    let cell = vec2u(v.pos.xy);
    let i = cell.y * u32(pn.resolution.x) + cell.x;
    let span = max(params.far - params.near, 1.0);
    // Where the stream stands, in passes of the span; a streak's own phase shifts it.
    let flown = params.beats * step * params.speed / span;
    let phase = rand(i, 0u, 7u).x;
    let at = flown + phase;
    let lap = u32(i32(floor(at)) & 0x7fffffff);
    let y = params.far - fract(at) * span;
    // A free place across: up to four tries, else none this lap.
    var x = 0.0;
    var z = 0.0;
    var found = false;
    for (var k = 0u; k < 4u; k++) {
        let r = rand(i, lap, k + 1u);
        let cx = (2.0 * r.x - 1.0) * params.spread;
        let cz = MARGIN + r.y * (CEILING - 2.0 * MARGIN);
        if !found && free(cx, cz) {
            x = cx;
            z = cz;
            found = true;
        }
    }
    // Thinning: each streak flies while its own share is below the stream's share. The edge is
    // soft, so a streak shortens and grows back rather than popping.
    let share = clamp(params.presence, 0.0, 1.0) * mix(params.density, 1.0, clamp(params.onset, 0.0, 1.0));
    let own = clamp((share - rand(i, lap, 9u).z) * 10.0, 0.0, 1.0);
    let len = params.length * clamp(params.bpm, 40.0, 240.0) / 120.0 * own;
    if !found || len < 0.01 || clamp(params.presence, 0.0, 1.0) < 0.001 {
        return vec4f(x, y, z, 0.0);
    }
    return vec4f(x, y, z, len);
}
