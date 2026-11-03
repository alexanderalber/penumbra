// Palms beside the lane of the synthwave stage, drawn by two geo.tex_instance nodes: one with
// trunk.obj (a texel per palm) and one with frond.obj (a texel per frond). A row of texels is a
// palm's place along the floor. The palms stand in a line on each side of the lane, `spacing`
// bars of travel apart, so a palm on the left passes the camera on a downbeat and one on the
// right half way between. They follow the lane at a fixed distance from its edge and lean away
// from it. Their fronds sway with the mids, each a little out of step with the others.
// in0: the stage's lane picture (its `lane` output): red the lane's x, green its sideways slope
// per metre ahead, blue the metres per beat, alpha the floor (0 in a gap), texel i covering
// y = -8 + i * 0.25 to -8 + (i + 1) * 0.25 metres ahead. Unconnected: a straight lane at x = 0,
// no gaps and `step` metres per beat.
// `part` trunk: render 2 x 16, column 0 the left line, column 1 the right one. frond: render
// 16 x 16, columns 0 to 7 the fronds of the left palm, 8 to 15 those of the right one.
// `what` place: where the model's origin goes (the trunk's foot, the frond's start on the
// crown). aim: where its +y points. scale: its size (0 where nothing stands).
// @param part choice(trunk|frond) = trunk -- which model this node places
// @param what choice(place|aim|scale) = place -- what this node writes
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param drift float = 0 -100..100 -- where the bars stand against `beats`, in bars (drift of a chan.beat_grid)
// @param presence float = 1 0..1 -- how many palms stand: 1 all, towards 0 they shrink into the floor one after another, 0 none
// @param mid float = 0 0..1 -- the mid level (energy_mid of a chan.beat_grid): the fronds sway with it
// @param spacing float = 2 0.25..8 -- bars of travel between two palms of a line
// @param beats_per_bar float = 4 2..8 -- beats in a bar
// @param size float = 1 0.3..1.6 -- size of a palm: 1 is 2.6 m to the crown (the camera is 1.5 to 2.9 m up, the ceiling 4 m)
// @param fronds int = 7 3..8 -- fronds on each palm
// @param sway float = 14 0..40 -- degrees the fronds swing up and down at full mids
// @param calm float = 0.25 0..1 -- share of the sway left when the mids are quiet
// @param droop float = 8 -30..40 -- degrees the fronds hang below level, before their own curve
// @param lane float = 1.6 0..5 -- half the width of the free lane
// @param clear float = 1.5 0..5 -- metres between the lane's edge and a trunk, room for the fronds
// @param near float = -2 -8..10 -- where the palms leave, behind the camera
// @param far float = 14 1..56 -- where they appear
// @param step float = 0.5 0.1..2 -- metres per beat while no lane picture is wired; with one, the stage's step

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;
// Metres a trunk's foot stands above the floor.
const LIFT: f32 = 0.02;
// trunk.obj: the crown's height and how far it leans towards +y, at size 1.
const CROWN: f32 = 2.6;
const LEAN: f32 = 0.45;
const DEG: f32 = 0.0174533;

struct Palm {
    foot: vec3f,
    // Where the trunk leans to (+y of trunk.obj), level.
    lean: vec3f,
    size: f32,
    // A number of its own, 0 to 1, and its turn of the fronds.
    r: vec3f,
}

fn palm(side: u32, row: u32, lane0: vec4f) -> Palm {
    var p: Palm;
    let wired = lane0.b > 0.0;
    let step = select(params.step, lane0.b, wired);
    let bpb = max(params.beats_per_bar, 1.0);
    let spacing = max(params.spacing, 0.05);
    let gap_y = spacing * bpb * step;
    // Palm n of a line stands at y = gap_y * (n - travel); the right line half a gap later.
    let travel = (params.beats / bpb + params.drift) / spacing + 0.5 * f32(side);
    let whole = floor(travel);
    let part = travel - whole;
    let first = ceil(params.near / gap_y + part);
    let k = first + f32(row);
    let y = gap_y * (k - part);
    let n = i32(whole + k);
    p.r = vec3f(pn_hash(vec3u(bitcast<u32>(n), side, 47u))) / 4294967296.0;
    let u = (y - LANE_NEAR) / (LANE_FAR - LANE_NEAR);
    let lane = select(vec4f(0.0, 0.0, 0.0, 1.0), textureSampleLevel(in0, pn_clamp, vec2f(u, 0.5), 0.0), wired);
    let sign = select(-1.0, 1.0, side == 1u);
    let s = params.size * mix(0.85, 1.15, p.r.x);
    let off = params.lane * (1.0 + 0.5 * abs(lane.g)) + params.clear * s;
    p.foot = vec3f(lane.r + sign * off, y, LIFT);
    // Lean outwards, away from the lane, and a little along it.
    let a = (p.r.y - 0.5) * 1.2;
    p.lean = vec3f(sign * cos(a), sin(a), 0.0);
    // Presence: each palm has its own share of it, so as presence falls the palms shrink into
    // the floor one after another. A palm over a gap in the floor goes with it.
    let own = clamp(params.presence * 2.0 - p.r.z, 0.0, 1.0);
    var size = s * own * own * (3.0 - 2.0 * own) * clamp(lane.a, 0.0, 1.0);
    // A palm under 3 cm is left out.
    if y > params.far || size * CROWN < 0.03 {
        size = 0.0;
    }
    p.size = size;
    return p;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let cell = vec2u(v.pos.xy);
    if params.part == part_trunk {
        if cell.x > 1u {
            return vec4f(0.0);
        }
        let p = palm(cell.x, cell.y, lane0);
        if params.what == what_place {
            return vec4f(p.foot, 1.0);
        }
        if params.what == what_aim {
            return vec4f(p.lean, 1.0);
        }
        return vec4f(vec3f(p.size), 1.0);
    }
    let side = cell.x / 8u;
    let i = cell.x % 8u;
    let count = u32(clamp(params.fronds, 3, 8));
    let p = palm(side, cell.y, lane0);
    if side > 1u || i >= count {
        return select(vec4f(0.0, 1.0, 0.0, 1.0), vec4f(0.0), params.what != what_aim);
    }
    // The crown: the top of the trunk, which leans towards `lean`.
    let crown = p.foot + p.size * (CROWN * vec3f(0.0, 0.0, 1.0) + LEAN * p.lean);
    if params.what == what_place {
        return vec4f(crown, 1.0);
    }
    if params.what == what_scale {
        return vec4f(vec3f(p.size), 1.0);
    }
    // The frond's heading round the crown, and its tilt: hanging by `droop`, swinging up and
    // down with the mids, one swing per two beats, each frond at its own phase.
    let turn = 6.2831853 * (f32(i) + 0.3 * p.r.y) / f32(count) + 6.2831853 * p.r.x;
    let f = f32(i) / f32(count);
    let swing = sin(3.14159265 * params.beats + 6.2831853 * (f + p.r.z));
    let strength = mix(clamp(params.calm, 0.0, 1.0), 1.0, clamp(params.mid, 0.0, 1.0));
    let tilt = (-params.droop + params.sway * strength * swing) * DEG;
    return vec4f(cos(tilt) * cos(turn), cos(tilt) * sin(turn), sin(tilt), 1.0);
}
