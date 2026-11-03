// The tube of the tunnel stage, for geo.tex_instance: a ring on every beat of the distance
// travelled, the tube's black wall between two rings, and long lines along the wall from ring to
// ring. The tube's bottom lies on the floor (z = 0) and its axis `radius` metres above it, along
// y. The rings come nearer with the world, `step` metres per beat. The long lines roll around the
// axis with the lane: where the lane swings x metres to the side, they are turned by `twist`
// times x over the radius, so the lane always lies at the tube's bottom.
// in0: the stage's twist picture (lane.wgsl, `what` twist): red the lane's swing in metres,
// alpha the tube (1 where it stands, 0 in a gap).
// Rings and walls: one texel per ring, render 64 x 1; ring k stands on beat `whole` - 4 + k.
// Lines: one texel per piece of a line between two rings, render 64 x 24; column k is the piece
// from ring k to ring k + 1, row j the line.
// `what` ring_place: the ring's middle (and the wall's start). ring_scale: the ring's size (0 in a
// gap). ring_light: its colour. wall_scale: the wall's size (radius across, length along y).
// line_place: the middle of a piece of line. line_aim: its direction. line_scale: `thickness`
// across, its length along y. line_light: its colour.
// @param what choice(ring_place|ring_scale|ring_light|wall_scale|line_place|line_aim|line_scale|line_light) = ring_place -- what this node writes
// @param whole float = 0 0..20000 -- the whole beats travelled (travelled of a chan.beat_grid less its fraction)
// @param phase float = 0 0..1 -- the fraction of the beat travelled
// @param drift float = 0 -100..100 -- where the bars stand against the beats, in bars (drift of a chan.beat_grid)
// @param bass float = 0 0..1 -- the bass (energy_low of a chan.beat_grid): the rings brighten with it
// @param step float = 0.5 0.1..2 -- metres the world moves per beat; the distance between two rings
// @param beats_per_bar float = 4 2..8 -- beats in a bar: the ring on a downbeat is brighter
// @param radius float = 4 1..10 -- the tube's radius, metres
// @param lines int = 18 0..24 -- long lines along the wall
// @param twist float = 1.5 0..5 -- how far the tube rolls with the lane: 1 turns it by the lane's swing over the radius
// @param thickness float = 0.015 0.001..0.1 -- metres across a long line
// @param warmth float = 0 0..1 -- 0 the warm colour, 1 the cold one

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;
const RINGS: u32 = 64u;
const LINES_MAX: u32 = 24u;
// Rings behind the camera: the nearest one stands a few beats behind it.
const BEHIND: f32 = 4.0;
// The wall stands this much outside the rings and lines, so they lie in front of it.
const WALL_OUT: f32 = 0.06;
const TAU: f32 = 6.2831853;

// Metres ahead of the camera of ring k.
fn ring_y(k: f32) -> f32 {
    return (k - BEHIND - params.phase) * params.step;
}

fn lane_at(y: f32) -> vec4f {
    let u = (y - LANE_NEAR) / (LANE_FAR - LANE_NEAR);
    let l = textureSampleLevel(in0, pn_clamp, vec2f(u, 0.5), 0.0);
    // Unwired, the lane picture is black: a straight lane and a whole tube.
    let wired = textureLoad(in0, vec2i(0, 0), 0).b > 0.0;
    return select(vec4f(0.0, 0.0, 0.0, 1.0), l, wired);
}

// The tube between ring k and ring k + 1: 1 where it stands, 0 in a gap. Sampled in the middle
// between the rings, where the lane picture's texels lie wholly inside that stretch.
fn tube(k: f32) -> f32 {
    return clamp(lane_at(ring_y(k) + 0.5 * params.step).a, 0.0, 1.0);
}

// The roll of the tube at ring k, radians.
fn roll(k: f32) -> f32 {
    let r = max(params.radius, 0.1);
    return -params.twist * lane_at(ring_y(k)).r / r;
}

// Where line j meets ring k.
fn on_wall(k: f32, j: f32) -> vec3f {
    let n = f32(clamp(params.lines, 1, 24));
    let a = TAU * j / n + roll(k);
    let r = params.radius;
    return vec3f(r * sin(a), ring_y(k), r - r * cos(a));
}

fn warm_cold(warm: vec3f, cold: vec3f) -> vec3f {
    return mix(warm, cold, clamp(params.warmth, 0.0, 1.0));
}

// Brighter on a downbeat: the beat of ring k starts a bar.
fn downbeat(k: f32) -> f32 {
    let bpb = max(params.beats_per_bar, 1.0);
    let beat = params.whole - BEHIND + k;
    let q = beat / bpb + params.drift + 0.5 / bpb;
    return select(1.0, 1.6, q - floor(q) < 1.0 / bpb);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let cell = vec2u(v.pos.xy);
    if cell.x >= RINGS || cell.y >= LINES_MAX {
        return vec4f(0.0, 0.0, 0.0, 1.0);
    }
    let k = f32(cell.x);
    let r = params.radius;
    let bass = clamp(params.bass, 0.0, 1.0);

    if params.what == what_ring_place {
        return vec4f(0.0, ring_y(k), r, 1.0);
    }
    if params.what == what_ring_scale || params.what == what_ring_light {
        // A ring stands where the tube stands on either side of it.
        let here = max(tube(k - 1.0), tube(k));
        if params.what == what_ring_scale {
            let s = select(r, 0.0, here < 0.02);
            return vec4f(s, 1.0, s, 1.0);
        }
        let c = warm_cold(vec3f(1.0, 0.62, 0.32), vec3f(0.55, 0.8, 1.0));
        return vec4f(c * here * (0.55 + 0.5 * bass) * downbeat(k), 1.0);
    }
    if params.what == what_wall_scale {
        // The wall shortens from its far end while a gap opens.
        let here = tube(k);
        let w = select(r + WALL_OUT, 0.0, here < 0.02);
        return vec4f(w, params.step * here, w, 1.0);
    }

    // The long lines.
    let j = f32(cell.y);
    let shown = i32(cell.y) < params.lines;
    let a = on_wall(k, j);
    let b = on_wall(k + 1.0, j);
    if params.what == what_line_place {
        return vec4f(0.5 * (a + b), 1.0);
    }
    if params.what == what_line_aim {
        return vec4f(b - a, 1.0);
    }
    let here = tube(k);
    if params.what == what_line_scale {
        let on = shown && here >= 0.02;
        let t = select(0.0, params.thickness, on);
        return vec4f(t, select(0.0, length(b - a), on), t, 1.0);
    }
    let c = warm_cold(vec3f(0.85, 0.42, 0.62), vec3f(0.45, 0.55, 1.0));
    return vec4f(c * here * (0.6 + 0.25 * bass), 1.0);
}
