// The tower of the ascent stage, for geo.tex_instance: storeys passing downwards as the camera
// climbs, and posts and braces between them. The tower stands around a vertical axis `radius`
// / 2 ahead of the camera, so the camera rises inside its shaft, halfway between the axis and
// the wall. Each storey is a ring-shaped floor along the wall, open in the middle, `storey`
// metres above the one below. One storey passes every `storey_beats` beats of the distance
// travelled. The tower turns slowly around its axis, once in `orbit` beats, and sways with the
// lane: where the lane swings x metres to the side, it turns by `twist` times x over the radius.
// So the camera seems to circle inside the shaft.
// in0: the stage's sway picture (lane.wgsl, `what` twist): red the lane's swing in metres.
// Storeys: one texel per storey, render 12 x 1; storey k is the one k - 3 above the camera's.
// Posts: one texel per piece between two storeys, render 12 x 48; column k is the piece from
// storey k to storey k + 1, rows 0 to 23 the posts, rows 24 to 47 the braces.
// `what` storey_place: the storey's middle (on the axis). storey_aim: its turn. storey_scale:
// its size (0 in a gap). storey_light: its colour. post_place: the middle of a piece of post or
// brace. post_aim: its direction. post_scale: `thickness` across, its length along y.
// post_light: its colour.
// @param what choice(storey_place|storey_aim|storey_scale|storey_light|post_place|post_aim|post_scale|post_light) = storey_place -- what this node writes
// @param whole float = 0 0..20000 -- the whole beats travelled (travelled of a chan.beat_grid less its fraction)
// @param phase float = 0 0..1 -- the fraction of the beat travelled
// @param bass float = 0 0..1 -- the bass (energy_low of a chan.beat_grid): the storeys brighten with it
// @param storey float = 3 1..10 -- metres from one storey to the next
// @param storey_beats float = 4 1..16 -- beats the camera takes to climb one storey
// @param radius float = 6 2..20 -- the tower's radius, metres; the models are made for 6
// @param lines int = 18 0..24 -- posts around the tower
// @param twist float = 1 0..5 -- how far the tower sways with the lane: 1 turns it by the lane's swing over the radius
// @param orbit float = 128 8..1024 -- beats the tower takes to turn once around its axis
// @param thickness float = 0.02 0.001..0.1 -- metres across a post
// @param warmth float = 0 0..1 -- 0 the warm colour, 1 the cold one
// @param gap float = 0 0..1 -- share of the storeys that break off into gaps: 0 none, 1 all of them
// @param gap_length float = 8 1..40 -- metres a gap runs up the tower, rounded to whole storeys

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;
const STOREYS: u32 = 12u;
const ROWS: u32 = 48u;
const POSTS_MAX: u32 = 24u;
// Storeys below the camera's: the lowest one drawn stands this many storeys down.
const BELOW: f32 = 3.0;
// The radius the models are made for.
const MODEL_RADIUS: f32 = 6.0;
// Share of a gap's growth over which it opens: a gap fades in as `gap` rises past its draw.
const GAP_SOFT: f32 = 0.08;
const TAU: f32 = 6.2831853;

// Storeys climbed: the whole ones and the fraction of the one under way.
fn climbed() -> vec2f {
    let sb = max(params.storey_beats, 0.25);
    // `whole` is a whole number, so its remainder is exact in f32.
    let rest = params.whole - sb * floor(params.whole / sb) + params.phase;
    let past = floor(params.whole / sb) + floor(rest / sb);
    let f = rest / sb - floor(rest / sb);
    return vec2f(past, f);
}

// Metres above the camera's floor of storey k.
fn storey_z(k: f32) -> f32 {
    return (k - BELOW - climbed().y) * params.storey;
}

// The storey's number counted from the start of the climb.
fn storey_number(k: f32) -> f32 {
    return climbed().x + k - BELOW;
}

// The middle of the tower, ahead of the camera.
fn axis() -> vec2f {
    return vec2f(0.0, 0.5 * params.radius);
}

fn lane_x() -> f32 {
    let u = (0.0 - LANE_NEAR) / (LANE_FAR - LANE_NEAR);
    let l = textureSampleLevel(in0, pn_clamp, vec2f(u, 0.5), 0.0);
    // Unwired, the sway picture is black: no sway.
    let wired = textureLoad(in0, vec2i(0, 0), 0).b > 0.0;
    return select(0.0, l.r, wired);
}

// The tower's turn, radians: the slow orbit and the sway with the lane.
fn turn() -> f32 {
    let o = max(params.orbit, 1.0);
    let lap = (params.whole - o * floor(params.whole / o) + params.phase) / o;
    return TAU * lap + params.twist * lane_x() / max(params.radius, 0.1);
}

// The storey numbered n: 1 where it stands, 0 in a gap. The storeys are cut into stretches of
// about `gap_length`; a stretch is a gap when its draw lies under `gap`.
fn standing(n: f32) -> f32 {
    if params.gap <= 0.0 {
        return 1.0;
    }
    let per = max(round(params.gap_length / max(params.storey, 0.1)), 1.0);
    let stretch = floor(n / per);
    let draw = f32(pn_hash(vec3u(bitcast<u32>(i32(stretch)), 31u, 7u)).x) / 4294967296.0;
    let open = smoothstep(0.0, GAP_SOFT, params.gap * (1.0 + GAP_SOFT) - draw);
    return 1.0 - open;
}

// Where post j meets storey k.
fn on_wall(k: f32, j: f32) -> vec3f {
    let n = f32(clamp(params.lines, 1, 24));
    let a = TAU * j / n + turn();
    let c = axis();
    let r = params.radius;
    return vec3f(c.x + r * sin(a), c.y + r * cos(a), storey_z(k));
}

fn warm_cold(warm: vec3f, cold: vec3f) -> vec3f {
    return mix(warm, cold, clamp(params.warmth, 0.0, 1.0));
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let cell = vec2u(v.pos.xy);
    if cell.x >= STOREYS || cell.y >= ROWS {
        return vec4f(0.0, 0.0, 0.0, 1.0);
    }
    let k = f32(cell.x);
    let bass = clamp(params.bass, 0.0, 1.0);

    if params.what == what_storey_place {
        let c = axis();
        return vec4f(c.x, c.y, storey_z(k), 1.0);
    }
    if params.what == what_storey_aim {
        let a = turn();
        return vec4f(sin(a), cos(a), 0.0, 1.0);
    }
    let n = storey_number(k);
    let here = standing(n);
    if params.what == what_storey_scale {
        let s = select(params.radius / MODEL_RADIUS, 0.0, here < 0.02);
        return vec4f(s, s, select(1.0, 0.0, here < 0.02), 1.0);
    }
    if params.what == what_storey_light {
        // Every fourth storey is brighter: a phrase of four storeys.
        let fourth = n - 4.0 * floor(n / 4.0) < 0.5;
        let c = warm_cold(vec3f(1.0, 0.62, 0.32), vec3f(0.55, 0.8, 1.0));
        return vec4f(c * here * (0.55 + 0.5 * bass) * select(1.0, 1.6, fourth), 1.0);
    }

    // Posts in rows 0 to 23, braces in rows 24 to 47.
    let brace = cell.y >= POSTS_MAX;
    let j = f32(cell.y % POSTS_MAX);
    let shown = i32(cell.y % POSTS_MAX) < params.lines;
    // A brace runs to the next post, leaning the other way on every other storey.
    let odd = n - 2.0 * floor(n / 2.0) > 0.5;
    var a = on_wall(k, j);
    var b = on_wall(k + 1.0, j);
    if brace {
        a = on_wall(k, select(j, j + 1.0, odd));
        b = on_wall(k + 1.0, select(j + 1.0, j, odd));
    }
    if params.what == what_post_place {
        return vec4f(0.5 * (a + b), 1.0);
    }
    if params.what == what_post_aim {
        return vec4f(b - a, 1.0);
    }
    if params.what == what_post_scale {
        let t = select(0.0, params.thickness, shown);
        return vec4f(t, select(0.0, length(b - a), shown), t, 1.0);
    }
    let c = warm_cold(vec3f(0.85, 0.42, 0.62), vec3f(0.45, 0.55, 1.0));
    return vec4f(c * (0.6 + 0.25 * bass) * select(1.0, 0.55, brace), 1.0);
}
