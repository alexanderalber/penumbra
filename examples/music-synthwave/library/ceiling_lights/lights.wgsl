// Rows of paper lanterns under the ceiling of the synthwave stage, one row on each side of the
// lane, drawn by geo.tex_instance with lantern.obj (string, ribs and rings, as lines) and
// paper.obj (the paper, filled in the lantern's colour, dim while dark, bright while lit). The
// ribs lie on the paper as darker lines. The lanterns swing
// a little, more with the mids. A lantern hangs every `spacing` beats of travel. A running light
// switches along each row: on every beat the next lantern ahead lights up, the ones it left glow
// on dimmer for a beat or two, so the light runs away from the camera faster than the floor
// comes. The pattern repeats every `period` lanterns; the right row runs half a period behind the
// left one. Each lantern in a period takes its own colour from the warm or cold palette, as the
// sun does. In a drop every lantern is on.
// in0: the stage's lane picture (its `lane` output): red the lane's x, blue the metres per
// beat, texel i covering y = -8 + i * 0.25 to -8 + (i + 1) * 0.25 metres ahead. Unconnected: a
// straight lane at x = 0 and `step` metres per beat.
// Render it 24 x 2: row 0 the left lanterns, row 1 the right ones, nearest first.
// `what` place: where the lantern hangs from the ceiling. aim: its swing. scale: its size (0
// where none hangs). shade: the colour of its ribs. light: the colour of its paper.
// @param what choice(place|aim|scale|shade|light) = place -- what this node writes
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param beat_index float = 0 0..1000000 -- beats counted (beat_index of a chan.beat_grid): the running light steps on each
// @param beat float = 0 0..1 -- how far the current beat has gone (beat of a chan.beat_grid)
// @param role float = 0 0..4 -- the section's role (section_role of a chan.beat_grid): 3, a drop, puts every lantern on
// @param drop float = 0 0..1 -- a flash at a drop (drop of a chan.beat_grid): every lantern lights up with it
// @param presence float = 1 0..1 -- how many lanterns hang: 1 all, towards 0 they draw up into the ceiling one after another, 0 none
// @param spacing float = 2 1..8 -- beats of travel between two lanterns of a row
// @param period int = 8 3..16 -- lanterns from one running light to the next
// @param trail int = 3 0..4 -- lanterns behind the running light that still glow
// @param apart float = 2.4 0..6 -- metres from the lane's middle to each row (beside the lane, since a lantern hangs down to 3 m)
// @param size float = 1 0.3..1.2 -- a lantern's size: 1 hangs 0.32 m from the ceiling on its string and reaches 0.98 m down with its tassel
// @param idle float = 0.12 0..1 -- a lantern's paper while it is off
// @param full float = 0.8 0..2 -- every lantern's light in a drop
// @param glow float = 1 0..4 -- how bright a lit lantern's paper shines
// @param swing float = 7 0..30 -- degrees a lantern swings to and fro at full mids
// @param calm float = 0.3 0..1 -- the share of the swing left without mids
// @param mids float = 0 0..1 -- the mids (energy_mid of a chan.beat_grid, smoothed): the lanterns swing with them
// @param warmth float = 0 0..1 -- 0 the warm palette, 1 the cold one, as the stage's
// @param ceiling float = 4 1..10 -- metres up where the lanterns hang from (the stage's ceiling grid)
// @param near float = -2 -8..10 -- where the lanterns leave, behind the camera
// @param far float = 14 1..56 -- where they appear
// @param step float = 0.5 0.1..2 -- metres per beat while no lane picture is wired; with one, the stage's step

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;

// The palette: warm (0) runs yellow, orange, magenta; cold (1) cyan, blue, violet. `s` 0 to 1.
// `pick` 0 to 1, fixed per lantern: between warm and cold each lantern takes one palette whole,
// the share of cold ones growing with `warmth`, so paper never mixes to grey.
fn palette(s: f32, pick: f32) -> vec3f {
    let warm = mix(mix(vec3f(1.0, 0.86, 0.3), vec3f(1.0, 0.42, 0.18), smoothstep(0.0, 0.5, s)), vec3f(0.95, 0.12, 0.55), smoothstep(0.45, 1.0, s));
    let cold = mix(mix(vec3f(0.35, 0.95, 1.0), vec3f(0.25, 0.45, 1.0), smoothstep(0.0, 0.5, s)), vec3f(0.62, 0.18, 1.0), smoothstep(0.45, 1.0, s));
    return select(warm, cold, pick < clamp(params.warmth, 0.0, 1.0));
}

fn wrap(a: i32, n: i32) -> i32 {
    return ((a % n) + n) % n;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let cell = vec2u(v.pos.xy);
    let side = cell.y;
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let wired = lane0.b > 0.0;
    let step = select(params.step, lane0.b, wired);
    let spacing = max(params.spacing, 0.25);
    let gap_y = spacing * step;
    // Lantern n hangs at y = gap_y * (n - travel). Only the travel's fraction moves the lanterns,
    // kept apart from the whole count to stay exact.
    let travel = params.beats / spacing;
    let whole = floor(travel);
    let part = travel - whole;
    let first = ceil(params.near / gap_y + part);
    let k = first + f32(cell.x);
    let y = gap_y * (k - part);
    let n = i32(whole + k);
    let u = (y - LANE_NEAR) / (LANE_FAR - LANE_NEAR);
    let lane = select(0.0, textureSampleLevel(in0, pn_clamp, vec2f(u, 0.5), 0.0).r, wired);
    let sign = select(-1.0, 1.0, side == 1u);
    if params.what == what_place {
        return vec4f(lane + sign * params.apart, y, params.ceiling - 0.01, 1.0);
    }
    let r = vec3f(pn_hash(vec3u(bitcast<u32>(n), side, 71u))) / 4294967296.0;
    if params.what == what_aim {
        // A swing to and fro along the lane, about every 3 beats, each lantern at its own phase.
        let amount = params.swing * mix(params.calm, 1.0, clamp(params.mids, 0.0, 1.0));
        let a = radians(amount) * sin(6.2831853 * (params.beats / 3.0 + r.y));
        return vec4f(0.0, cos(a), sin(a), 1.0);
    }
    // Presence: each lantern has its own share of it, so as presence falls the lanterns draw up
    // into the ceiling one after another.
    let own = clamp(params.presence * 2.0 - r.x, 0.0, 1.0);
    var size = clamp(params.size, 0.0, 1.2) * own * own * (3.0 - 2.0 * own);
    if y > params.far || size < 0.02 {
        size = 0.0;
    }
    if params.what == what_scale {
        return vec4f(vec3f(size), 1.0);
    }
    // The running light: lantern `lead` is lit, the `trail` lanterns behind it glow on dimmer. It
    // steps one lantern ahead on each beat.
    let period = clamp(params.period, 3, 16);
    let lead = i32(floor(params.beat_index + 0.5)) + i32(side) * (period / 2);
    let behind = wrap(lead - n, period);
    var lit = 0.0;
    if behind == 0 {
        lit = 1.0 - 0.3 * clamp(params.beat, 0.0, 1.0);
    } else if behind <= clamp(params.trail, 0, 4) {
        lit = 0.5 * pow(0.45, f32(behind - 1)) * (1.0 - 0.4 * clamp(params.beat, 0.0, 1.0));
    }
    // A drop puts every lantern on, and its flash lights them all up.
    var every = clamp(params.drop, 0.0, 1.0) * 1.5;
    if abs(params.role - 3.0) < 0.5 {
        every = max(every, params.full);
    }
    lit = max(lit, every);
    let colour = palette(f32(wrap(n, period)) / f32(period - 1), r.z);
    if params.what == what_shade {
        // The ribs: darker than lit paper, a little lighter than dark paper.
        return vec4f(colour * (0.3 + 0.2 * min(lit, 1.0)), 1.0);
    }
    // The paper: dim in the lantern's colour while dark, bright and a little whiter while lit.
    let shine = min(lit, 1.2) * params.glow;
    return vec4f(mix(colour, vec3f(1.0), 0.12 * min(shine, 1.0)) * (params.idle + 1.1 * shine), 1.0);
}
