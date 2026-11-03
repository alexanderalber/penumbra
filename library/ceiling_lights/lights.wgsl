// Rows of lamps under the ceiling of the synthwave stage, one row on each side of the lane,
// drawn by geo.tex_instance with lamp.obj (a rod and a shade, as lines), bulb.obj and rays.obj
// (short rays of light under a lit lamp). A lamp hangs every `spacing` beats of travel. A running light
// switches along each row: on every beat the next lamp ahead lights up, the ones it left glow
// on dimmer for a beat or two, so the light runs away from the camera faster than the floor
// comes. The pattern repeats every `period` lamps; the right row runs half a period behind the
// left one. Each lamp in a period takes its own colour from the warm or cold palette, as the
// sun does. In a drop every lamp is on.
// in0: the stage's lane picture (its `lane` output): red the lane's x, blue the metres per
// beat, texel i covering y = -8 + i * 0.25 to -8 + (i + 1) * 0.25 metres ahead. Unconnected: a
// straight lane at x = 0 and `step` metres per beat.
// Render it 24 x 2: row 0 the left lamps, row 1 the right ones, nearest first.
// `what` place: where the lamp hangs from the ceiling. scale: its size (0 where none hangs).
// shade: the colour of its lines. light: the bulb's colour. rays: the size of its rays (0 while
// the lamp is dark). glow: the rays' colour.
// @param what choice(place|scale|shade|light|rays|glow) = place -- what this node writes
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param beat_index float = 0 0..1000000 -- beats counted (beat_index of a chan.beat_grid): the running light steps on each
// @param beat float = 0 0..1 -- how far the current beat has gone (beat of a chan.beat_grid)
// @param role float = 0 0..4 -- the section's role (section_role of a chan.beat_grid): 3, a drop, puts every lamp on
// @param drop float = 0 0..1 -- a flash at a drop (drop of a chan.beat_grid): every lamp lights up with it
// @param presence float = 1 0..1 -- how many lamps hang: 1 all, towards 0 they draw up into the ceiling one after another, 0 none
// @param spacing float = 2 1..8 -- beats of travel between two lamps of a row
// @param period int = 8 3..16 -- lamps from one running light to the next
// @param trail int = 3 0..4 -- lamps behind the running light that still glow
// @param apart float = 2.4 0..6 -- metres from the lane's middle to each row (beside the lane, since a lamp hangs below 3.2 m)
// @param size float = 1 0.3..1.2 -- a lamp's size: 1 hangs 0.73 m from the ceiling, its rays reach 1.03 m down
// @param idle float = 0.12 0..1 -- a lamp's light while it is off
// @param full float = 0.8 0..2 -- every lamp's light in a drop
// @param glow float = 1 0..4 -- the light of the rays under a lit lamp
// @param warmth float = 0 0..1 -- 0 the warm palette, 1 the cold one, as the stage's
// @param ceiling float = 4 1..10 -- metres up where the lamps hang from (the stage's ceiling grid)
// @param near float = -2 -8..10 -- where the lamps leave, behind the camera
// @param far float = 14 1..56 -- where they appear
// @param step float = 0.5 0.1..2 -- metres per beat while no lane picture is wired; with one, the stage's step

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;

// The palette: warm (0) runs yellow, orange, magenta; cold (1) cyan, blue, violet. `s` 0 to 1.
fn palette(s: f32) -> vec3f {
    let warm = mix(mix(vec3f(1.0, 0.86, 0.3), vec3f(1.0, 0.42, 0.18), smoothstep(0.0, 0.5, s)), vec3f(0.95, 0.12, 0.55), smoothstep(0.45, 1.0, s));
    let cold = mix(mix(vec3f(0.35, 0.95, 1.0), vec3f(0.25, 0.45, 1.0), smoothstep(0.0, 0.5, s)), vec3f(0.62, 0.18, 1.0), smoothstep(0.45, 1.0, s));
    return mix(warm, cold, clamp(params.warmth, 0.0, 1.0));
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
    // Lamp n hangs at y = gap_y * (n - travel). Only the travel's fraction moves the lamps,
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
    // Presence: each lamp has its own share of it, so as presence falls the lamps draw up
    // into the ceiling one after another.
    let own = clamp(params.presence * 2.0 - r.x, 0.0, 1.0);
    var size = clamp(params.size, 0.0, 1.2) * own * own * (3.0 - 2.0 * own);
    if y > params.far || size < 0.02 {
        size = 0.0;
    }
    if params.what == what_scale {
        return vec4f(vec3f(size), 1.0);
    }
    // The running light: lamp `lead` is lit, the `trail` lamps behind it glow on dimmer. It
    // steps one lamp ahead on each beat.
    let period = clamp(params.period, 3, 16);
    let lead = i32(floor(params.beat_index + 0.5)) + i32(side) * (period / 2);
    let behind = wrap(lead - n, period);
    var lit = 0.0;
    if behind == 0 {
        lit = 1.0 - 0.3 * clamp(params.beat, 0.0, 1.0);
    } else if behind <= clamp(params.trail, 0, 4) {
        lit = 0.5 * pow(0.45, f32(behind - 1)) * (1.0 - 0.4 * clamp(params.beat, 0.0, 1.0));
    }
    // A drop puts every lamp on, and its flash lights them all up.
    var every = clamp(params.drop, 0.0, 1.0) * 1.5;
    if abs(params.role - 3.0) < 0.5 {
        every = max(every, params.full);
    }
    lit = max(lit, every);
    let colour = palette(f32(wrap(n, period)) / f32(period - 1));
    if params.what == what_shade {
        return vec4f(colour * mix(0.4, 1.0, min(lit, 1.0)), 1.0);
    }
    if params.what == what_light {
        return vec4f(colour * (params.idle + 2.0 * lit), 1.0);
    }
    if params.what == what_rays {
        // Rays only under a lamp that shines: dark ones would draw black lines.
        return vec4f(vec3f(select(0.0, size, lit * params.glow > 0.1)), 1.0);
    }
    return vec4f(colour * params.glow * min(lit, 1.2), 1.0);
}
