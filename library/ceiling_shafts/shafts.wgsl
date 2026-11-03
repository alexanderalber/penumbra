// Shafts of light falling slanted from openings in the ceiling of the synthwave stage, beside
// the lane, drawn by geo.tex_instance: rays_0.obj to rays_2.obj (the three parts of a cone of
// rays one unit long along -z, its narrow end at the origin), hole.obj (a square in the
// ceiling) and mote.obj (a speck of dust drifting in the light). The rays are lines: a cone of
// light whose light fades towards the floor, part by part. A shaft hangs every bar of travel, by turns left and right of the lane, so one
// passes the camera on each downbeat. The cones lean outwards and towards the camera, and the
// treble brightens them.
// in0: the stage's lane picture (its `lane` output): red the lane's x, blue the metres per
// beat, texel i covering y = -8 + i * 0.25 to -8 + (i + 1) * 0.25 metres ahead. Unconnected: a
// straight lane at x = 0 and `step` metres per beat.
// `part` cone: render 10 x 1, a texel per shaft (the rays and the hole share its place).
// dust: render 32 x 10, column m the mote, row k the shaft.
// `what` place: where the model's origin goes. aim: where its +y points. scale: its size (0
// where nothing hangs); hole: the hole's size. light: the colour part `band` of the rays, the
// hole or a mote shines in.
// @param part choice(cone|dust) = cone -- which model this node places
// @param what choice(place|aim|scale|hole|light) = place -- what this node writes
// @param band int = 0 0..3 -- for light: 0 to 2 the parts of the rays from the top, 3 the hole
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param drift float = 0 -100..100 -- where the bars stand against `beats`, in bars (drift of a chan.beat_grid)
// @param high float = 0 0..1 -- the treble (energy_high of a chan.beat_grid): the light brightens with it
// @param presence float = 1 0..1 -- how many shafts shine: 1 all, towards 0 they draw up into the ceiling one after another, 0 none
// @param spacing float = 1 0.25..8 -- bars of travel between two shafts, which alternate sides
// @param beats_per_bar float = 4 2..8 -- beats in a bar
// @param size float = 1 0.3..2 -- the cone's radius at the floor, metres
// @param narrow float = 0.3 0.05..1 -- the cone's radius at the ceiling, as a share of `size`
// @param slant float = 18 0..35 -- degrees the cones lean from straight down
// @param outwards float = 35 0..90 -- degrees the lean turns from towards the camera to away from the lane
// @param lane float = 1.6 0..5 -- half the width of the free lane
// @param clear float = 0.8 0..5 -- metres between the lane's edge and the nearest part of a cone
// @param base float = 0.5 0..2 -- the light without treble
// @param gain float = 1.2 0..4 -- the light the full treble adds
// @param warmth float = 0 0..1 -- 0 a warm light, 1 a cold one, as the stage's palette
// @param dust int = 24 0..32 -- motes of dust in each shaft
// @param mote float = 0.025 0.005..0.1 -- a mote's size, metres
// @param fall float = 0.012 0..0.1 -- shaft lengths per beat the dust sinks
// @param fade float = 5 0..20 -- metres ahead where the light starts to fade into the distance
// @param gone float = 11 1..30 -- metres ahead where it is gone
// @param ceiling float = 4 1..10 -- metres up where the openings are (the stage's ceiling grid)
// @param near float = -2 -8..10 -- where the shafts leave, behind the camera
// @param far float = 14 1..56 -- where they appear
// @param step float = 0.5 0.1..2 -- metres per beat while no lane picture is wired; with one, the stage's step

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;
const TAU: f32 = 6.2831853;
// Metres the lowest point of a cone's rim stays above the floor.
const LIFT: f32 = 0.02;

struct Shaft {
    top: vec3f,
    // Unit vectors of the cone's frame: +y (the aim) and +z of the rays' models.
    aim: vec3f,
    up: vec3f,
    // Radius at the floor, length, both shrunk by presence.
    radius: f32,
    length: f32,
    light: vec3f,
}

fn hash(a: i32, b: u32, c: u32) -> vec4f {
    let h = pn_hash(vec3u(bitcast<u32>(a), b, c));
    return vec4f(vec3f(h), f32(pn_hash(h).x)) / 4294967296.0;
}

fn shaft(k: u32) -> Shaft {
    var s: Shaft;
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let wired = lane0.b > 0.0;
    let step = select(params.step, lane0.b, wired);
    let bpb = max(params.beats_per_bar, 1.0);
    let spacing = max(params.spacing, 0.05);
    let gap_y = spacing * bpb * step;
    // Shaft n hangs at y = gap_y * (n - travel), so it passes y = 0 on a downbeat. Only the
    // travel's fraction moves the shafts, kept apart from the whole count to stay exact.
    let travel = (params.beats / bpb + params.drift) / spacing;
    let whole = floor(travel);
    let part = travel - whole;
    let first = ceil(params.near / gap_y + part);
    let kk = first + f32(k);
    let y = gap_y * (kk - part);
    let n = i32(whole + kk);
    let sign = select(-1.0, 1.0, ((n % 2) + 2) % 2 == 1);
    let r = hash(n, 0u, 61u);
    let u = (y - LANE_NEAR) / (LANE_FAR - LANE_NEAR);
    let lane = select(0.0, textureSampleLevel(in0, pn_clamp, vec2f(u, 0.5), 0.0).r, wired);
    // The lean: `slant` from straight down, turned from towards the camera to outwards.
    let a = radians(clamp(params.slant, 0.0, 60.0));
    let b = radians(params.outwards);
    let h = vec3f(sign * sin(b), -cos(b), 0.0);
    s.aim = vec3f(h.xy * cos(a), sin(a));
    s.up = vec3f(-h.xy * sin(a), cos(a));
    let radius = params.size * mix(0.85, 1.15, r.x);
    let narrow = clamp(params.narrow, 0.05, 1.0) * radius;
    // Long enough to reach the floor with the lowest point of its rim.
    let reach = max(params.ceiling - LIFT - radius * sin(a), 0.1) / cos(a);
    // The nearest part of the cone to the lane: its narrow end at the top, or its rim at the
    // floor where the lean takes it outwards less than the cone widens.
    let spread = max(narrow, radius - reach * sin(a) * sin(b));
    let x = lane + sign * (params.lane + params.clear + spread);
    s.top = vec3f(x, y, params.ceiling - 0.01);
    // Presence: each shaft has its own share of it, so as presence falls the shafts draw up
    // into the ceiling one after another.
    let own = clamp(params.presence * 2.0 - r.y, 0.0, 1.0);
    var grow = own * own * (3.0 - 2.0 * own);
    if y > params.far || grow < 0.01 {
        grow = 0.0;
    }
    s.radius = radius * grow;
    s.length = reach * grow;
    let warm = vec3f(1.0, 0.72, 0.42);
    let cold = vec3f(0.45, 0.8, 1.0);
    let colour = mix(warm, cold, clamp(params.warmth, 0.0, 1.0));
    let level = params.base + params.gain * clamp(params.high, 0.0, 1.0);
    let far = 1.0 - smoothstep(params.fade, max(params.gone, params.fade + 0.1), y);
    s.light = colour * level * far * mix(0.8, 1.2, r.z);
    return s;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let cell = vec2u(v.pos.xy);
    if params.part == part_cone {
        let s = shaft(cell.x);
        if params.what == what_place {
            return vec4f(s.top, 1.0);
        }
        if params.what == what_aim {
            return vec4f(s.aim, 1.0);
        }
        if params.what == what_scale {
            return vec4f(s.radius, s.radius, s.length, 1.0);
        }
        if params.what == what_hole {
            let side = 1.6 * clamp(params.narrow, 0.05, 1.0) * s.radius;
            return vec4f(side, side, 1.0, 1.0);
        }
        // The light fades towards the floor; the hole shines brightest.
        var share = array<f32, 4>(0.8, 0.45, 0.22, 1.6);
        return vec4f(s.light * share[clamp(params.band, 0, 3)], 1.0);
    }
    // A mote of dust: somewhere inside the cone, sinking slowly and turning round its axis.
    let s = shaft(cell.y);
    let n = u32(clamp(params.dust, 0, 32));
    let r = hash(i32(cell.y), cell.x, 67u);
    let t = fract(r.x + params.beats * params.fall);
    let turn = TAU * r.y + params.beats * 0.04 * (r.z - 0.5);
    let narrow = clamp(params.narrow, 0.05, 1.0);
    let out = sqrt(r.z) * 0.85 * mix(narrow, 1.0, t) * s.radius;
    let side = normalize(cross(s.aim, s.up));
    let at = s.top - t * s.length * s.up + out * (cos(turn) * side + sin(turn) * s.aim);
    // It glints now and then, and fades in at the top and out at the floor.
    let glint = pow(0.5 + 0.5 * sin(TAU * (r.w + 0.3 * params.beats)), 3.0);
    let ends = smoothstep(0.0, 0.1, t) * (1.0 - smoothstep(0.85, 1.0, t));
    var size = params.mote * ends;
    if cell.x >= n || s.length <= 0.0 {
        size = 0.0;
    }
    if params.what == what_place {
        return vec4f(at, 1.0);
    }
    if params.what == what_scale {
        return vec4f(vec3f(size), 1.0);
    }
    if params.what == what_aim {
        return vec4f(0.0, 1.0, 0.0, 1.0);
    }
    return vec4f(s.light * (0.6 + 2.4 * glint), 1.0);
}
