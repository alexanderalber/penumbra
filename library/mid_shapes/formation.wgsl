// The solids of mid_shapes, one per texel, for geo.tex_instance. They fly in a ring ahead of the
// camera, above its line of sight and the glider: a flat carousel `at` metres ahead and `hover`
// metres up that turns on by part of a step at every bar. Through a build the ring draws
// together; a drop throws the solids apart, and they gather again as the drop fades.
// in0: the stage's lane picture (its `lane` output); the ring follows the lane. Unconnected: a
// straight lane at x = 0.
// `what` place: x, y, z of each solid in rgb, its size in alpha (0 for none). aim: where its +y
// points, turning a quarter turn on each beat. Render it 8 x 1.
// @param what choice(place|aim) = place -- what this node writes
// @param bar_index float = 0 0..100000 -- bars counted (bar_index of a chan.beat_grid)
// @param bar float = 0 0..1 -- the bar's progress (bar of a chan.beat_grid)
// @param beat_index float = 0 0..1000000 -- beats counted (beat_index of a chan.beat_grid)
// @param beat float = 0 0..1 -- the beat's progress (beat of a chan.beat_grid)
// @param build float = 0 0..1 -- rises before a boundary (build of a chan.beat_grid): the ring draws together
// @param drop float = 0 0..1 -- a drop (drop of a chan.beat_grid): the solids fly apart
// @param presence float = 1 0..1 -- 1 all solids, towards 0 they shrink away one after another and the ring flies off ahead
// @param count int = 5 3..7 -- solids in the ring
// @param size float = 0.7 0.1..2 -- a solid's size across, metres
// @param at float = 6 4..12 -- metres ahead of the camera where the ring's middle flies
// @param hover float = 2.7 1.8..3.4 -- metres above the floor
// @param radius float = 2.4 0.5..5 -- the ring's half width, metres
// @param depth float = 1.2 0..4 -- the ring's half depth along the flight, metres
// @param tilt float = 0.35 0..1.5 -- metres the ring's far side stands higher than its near side
// @param tight float = 0.6 0..0.9 -- how much a full build draws the ring together
// @param scatter float = 3 0..8 -- how far a full drop throws the solids, in radii
// @param turn float = 0.5 0..1 -- the share of a step between two solids the ring turns on per bar
// @param spin float = 0.25 0..1 -- turns a solid makes on each beat

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;
const TAU: f32 = 6.2831853;
// Metres the ring flies further ahead and climbs at presence 0.
const AWAY: f32 = 8.0;
const CLIMB: f32 = 0.6;
// Where the solids stay between the floor and the ceiling, metres.
const LOW: f32 = 0.5;
const HIGH: f32 = 3.6;

fn rand(i: u32, k: u32) -> vec3f {
    return vec3f(pn_hash(vec3u(i, k, 41u))) / 4294967296.0;
}

// A step that is quick at first and settles: the kick of a beat or bar.
fn kick(f: f32) -> f32 {
    let g = 1.0 - clamp(f, 0.0, 1.0);
    return 1.0 - g * g * g * g;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let i = u32(v.pos.x);
    let r = rand(i, 1u);
    if params.what == what_aim {
        // A quarter turn (by `spin`) on each beat, settling before the next; a fixed lean per solid.
        let dir = select(-1.0, 1.0, r.x < 0.5);
        let beats = fract(params.beat_index * params.spin) + kick(params.beat) * params.spin;
        let a = TAU * (dir * beats + r.y);
        let b = 0.5 * sin(TAU * (0.13 * params.beat_index + r.z)) + 0.3;
        return vec4f(cos(a) * cos(b), sin(a) * cos(b), sin(b), 1.0);
    }
    let n = u32(clamp(params.count, 3, 7));
    let p = clamp(params.presence, 0.0, 1.0);
    let away = (1.0 - p) * (1.0 - p);
    // The ring turns on by `turn` of a step at every bar, quickly at the downbeat.
    let steps = (fract(params.bar_index * params.turn / f32(n)) * f32(n) + kick(params.bar) * params.turn);
    let a = TAU * (f32(i) + steps) / f32(n);
    let squeeze = 1.0 - params.tight * clamp(params.build, 0.0, 1.0);
    let drop = clamp(params.drop, 0.0, 1.0);
    let thrown = 1.0 + params.scatter * drop * mix(0.7, 1.3, r.z);
    let y0 = params.at + AWAY * away;
    var x = params.radius * squeeze * thrown * cos(a);
    var y = y0 + params.depth * squeeze * thrown * sin(a);
    var z = params.hover + CLIMB * away + params.tilt * sin(a) + 1.2 * drop * (r.x - 0.5);
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let lane = select(0.0, textureSampleLevel(in0, pn_clamp, vec2f((y0 - LANE_NEAR) / (LANE_FAR - LANE_NEAR), 0.5), 0.0).r, lane0.b > 0.0);
    x += lane;
    let size = params.size * mix(0.8, 1.15, r.y);
    z = clamp(z, LOW + 0.5 * size, HIGH - 0.5 * size);
    // Each solid has its own share of presence and shrinks away as presence falls below it.
    let own = clamp((p * 1.2 - 0.15 - 0.9 * f32(i) / f32(n)) * 6.0, 0.0, 1.0);
    var s = size * own * own * (3.0 - 2.0 * own);
    if i >= n || p < 0.001 {
        s = 0.0;
    }
    return vec4f(x, y, z, s);
}
