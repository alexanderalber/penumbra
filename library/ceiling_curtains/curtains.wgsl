// Curtains hanging from the ceiling of the synthwave stage across the lane, drawn by
// geo.tex_instance with segment.obj (the two side edges of a piece of cloth, one unit apart and
// one unit down from its top) and hem.obj (its lower edge). A curtain is a row of strips; a strip is 8 segments one under the other, each swung
// forward or back a little, so a wave runs down the cloth. A curtain hangs every `spacing` bars
// of travel, so one passes the camera on a downbeat. Over the lane the strips stay short, so
// the camera and the glider fly under them; beside the lane they hang long.
// in0: the stage's lane picture (its `lane` output): red the lane's x, blue the metres per
// beat, texel i covering y = -8 + i * 0.25 to -8 + (i + 1) * 0.25 metres ahead. Unconnected: a
// straight lane at x = 0 and `step` metres per beat.
// Render it 32 x 48: column i the strip, row 8 c + j segment j (0 at the top) of curtain c.
// `what` place: the top edge's middle of the segment. aim: where its +y points (the swing).
// scale: its width, 1, its length (0 where nothing hangs). hem: the same for the last segment
// of a strip, 0 for the others. light: a shade from its swing.
// @param what choice(place|aim|scale|hem|light) = place -- what this node writes
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param drift float = 0 -100..100 -- where the bars stand against `beats`, in bars (drift of a chan.beat_grid)
// @param mid float = 0 0..1 -- the mid level (energy_mid of a chan.beat_grid): the cloth waves with it
// @param presence float = 1 0..1 -- how much hangs: 1 all, towards 0 the strips draw up into the ceiling one after another, 0 none
// @param spacing float = 2 0.5..8 -- bars of travel between two curtains
// @param beats_per_bar float = 4 2..8 -- beats in a bar
// @param strips int = 25 3..32 -- strips across one curtain
// @param pitch float = 0.5 0.1..2 -- metres from one strip to the next
// @param fill float = 0.6 0.1..1 -- share of the pitch a strip covers
// @param short float = 0.7 0.05..0.8 -- metres a strip hangs over the lane (from a 4 m ceiling, 0.8 keeps it above 3.2 m)
// @param long float = 2.4 0.1..3.8 -- metres a strip hangs beside the lane
// @param open float = 2 1.6..6 -- metres from the lane's middle where the strips start to hang longer (the lane is 1.6 m to each side)
// @param foot float = 1.2 0.1..5 -- metres over which they grow from `short` to `long`
// @param wave float = 22 0..45 -- degrees the cloth swings at its hem at full mids
// @param calm float = 0.3 0..1 -- share of the swing left when the mids are quiet
// @param waves float = 1.2 0.2..4 -- waves from the top of a strip to its hem
// @param flow float = 0.25 0..2 -- waves per beat that run down the cloth
// @param ceiling float = 4 1..10 -- metres up where the curtains hang from (the stage's ceiling grid)
// @param near float = -2 -8..10 -- where the curtains leave, behind the camera
// @param far float = 14 1..56 -- where they appear
// @param step float = 0.5 0.1..2 -- metres per beat while no lane picture is wired; with one, the stage's step

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;
const SEGMENTS: u32 = 8u;
const TAU: f32 = 6.2831853;

struct Segment {
    top: vec3f,
    // The swing, radians: + moves the hem away from the camera.
    swing: f32,
    width: f32,
    length: f32,
}

fn hash(a: i32, b: u32, c: u32) -> vec3f {
    return vec3f(pn_hash(vec3u(bitcast<u32>(a), b, c))) / 4294967296.0;
}

fn segment(i: u32, c: u32, j: u32) -> Segment {
    var s: Segment;
    s.width = 0.0;
    s.length = 0.0;
    s.swing = 0.0;
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let wired = lane0.b > 0.0;
    let step = select(params.step, lane0.b, wired);
    let bpb = max(params.beats_per_bar, 1.0);
    let spacing = max(params.spacing, 0.05);
    let gap_y = spacing * bpb * step;
    // Curtain n hangs at y = gap_y * (n - travel), so it passes y = 0 on a downbeat. Only the
    // travel's fraction moves the curtains, kept apart from the whole count to stay exact.
    let travel = (params.beats / bpb + params.drift) / spacing;
    let whole = floor(travel);
    let part = travel - whole;
    let first = ceil(params.near / gap_y + part);
    let k = first + f32(c);
    let y = gap_y * (k - part);
    let n = i32(whole + k);
    let count = u32(clamp(params.strips, 3, 32));
    s.top = vec3f(0.0, y, params.ceiling);
    if i >= count || y > params.far {
        return s;
    }
    let u = (y - LANE_NEAR) / (LANE_FAR - LANE_NEAR);
    let lane = select(0.0, textureSampleLevel(in0, pn_clamp, vec2f(u, 0.5), 0.0).r, wired);
    let off = (f32(i) - 0.5 * f32(count - 1u)) * params.pitch;
    let x = lane + off;
    let r = hash(n, i, 53u);
    // Short over the lane, long beside it. The camera stays within the lane's swing of its
    // middle, so the long strips keep clear of it as well.
    let grow = smoothstep(params.open, params.open + params.foot, abs(off));
    var hang = mix(min(params.short, 0.8), params.long, grow) * mix(0.9, 1.08, r.x);
    // Presence: each strip has its own share of it, so as presence falls the strips draw up
    // into the ceiling one after another.
    let own = clamp(params.presence * 2.0 - r.y, 0.0, 1.0);
    hang *= own * own * (3.0 - 2.0 * own);
    if hang < 0.03 {
        return s;
    }
    let seg = hang / f32(SEGMENTS);
    // The swing grows towards the hem and runs down the cloth as a wave. Neighbouring strips
    // swing a little out of step, and every curtain has its own phase.
    let strength = mix(clamp(params.calm, 0.0, 1.0), 1.0, clamp(params.mid, 0.0, 1.0));
    let amp = radians(params.wave) * strength;
    let phase = 0.7 * f32(i) + TAU * r.z;
    var top = vec3f(x, y, params.ceiling - 0.01);
    for (var m = 0u; m < j; m++) {
        let a = swing(m, amp, phase);
        top += seg * vec3f(0.0, sin(a), -cos(a));
    }
    s.top = top;
    s.swing = swing(j, amp, phase);
    s.width = params.pitch * params.fill;
    s.length = seg;
    return s;
}

fn swing(m: u32, amp: f32, phase: f32) -> f32 {
    let t = (f32(m) + 0.5) / f32(SEGMENTS);
    return amp * t * sin(TAU * (params.waves * t - params.flow * params.beats) + phase);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let cell = vec2u(v.pos.xy);
    let s = segment(cell.x, cell.y / SEGMENTS, cell.y % SEGMENTS);
    if params.what == what_place {
        return vec4f(s.top, 1.0);
    }
    if params.what == what_aim {
        // segment.obj hangs along -z from its top; turning its +y up by the swing moves
        // its lower edge along +y.
        return vec4f(0.0, cos(s.swing), sin(s.swing), 1.0);
    }
    if params.what == what_scale {
        return vec4f(s.width, 1.0, s.length, 1.0);
    }
    if params.what == what_hem {
        let last = select(0.0, 1.0, cell.y % SEGMENTS == SEGMENTS - 1u);
        return vec4f(s.width * last, 1.0, s.length * last, 1.0);
    }
    // Cloth swung towards the camera catches more light than cloth swung away.
    return vec4f(vec3f(clamp(1.0 - 1.2 * sin(s.swing), 0.6, 1.3)), 1.0);
}
