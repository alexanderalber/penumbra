// The tube of the tunnel stage, for geo.tex_instance: a ring on every beat of the distance
// travelled, the tube's black wall between two rings, long lines along the wall from ring to
// ring, and chevrons on the tube's bottom. The tube's bottom lies on the floor (z = 0) and its
// axis `radius` metres above it, along y. The rings come nearer with the world, `step` metres
// per beat. The long lines roll around the axis with the lane: where the lane swings x metres to
// the side, they are turned by `twist` times x over the radius, so the lane always lies at the
// tube's bottom.
//
// The tube is a sheet rolled up: every ring, line and piece of wall has its place on a sheet
// `radius` x 2 pi wide. `roll_up` 1 rolls the sheet into the tube. Towards 0 it opens out into a
// flat floor of lines from right to left, the rings' pieces, which are all that is left at 0: the
// long lines, the wall and the chevrons come in only over the last part of the roll. A floor
// that rolls up into the tube needs the plane before it to show the same lines (stage_synthwave
// `roll_up`).
//
// Life on the beat: a pulse of light runs down the tube on every beat, from the camera ahead,
// and the rings it passes narrow a little. On every bar a bright dash runs along every third
// long line. Every `roll_bars` bars the tube makes a whole turn around its axis over
// `roll_length` bars, the next turn the other way; in a drop the turns come twice as often.
//
// Gaps: where the twist picture's alpha falls, the tube runs out over a few metres and comes
// back the same way. The rings thin out (every second ring stays, then every fourth, on the
// downbeats), the long lines and chevrons fade, and the wall opens from the top down in slats
// that narrow, so the sky shows through. In the open middle of a gap nothing is left.
//
// The turn up: `turn_up` 1 makes the tube a tower of hoops standing upright, the camera rising
// through it. Over its first third the long lines, dashes, chevrons, the pulse and the wall go
// (the wall opens from the top down as in a gap), and the rings thin out to one every
// `storey_beats` beats (every second ring goes first). From 0.2 to 0.8 the hoops move apart to
// `storey` metres and widen to `tower_radius`. From a third to 1 the tube turns upright about a
// line across the view, as far ahead of the camera's floor as up, so the hoops sink past the
// camera. stage_ascent lying down (its `turn_up` at the same share) draws the same hoops at the
// same places, so a cross between the two stages reads as one movement.
//
// in0: the stage's twist picture (lane.wgsl, `what` twist): red the lane's swing in metres,
// alpha the tube (1 where it stands whole, 0 in the open middle of a gap).
// Rings and walls: one texel per piece, render 64 x 48; column k is ring k (on beat `whole` - 4
// + k), row i its piece i of 48 around the tube. A wall piece lies between ring k and ring k + 1.
// Lines: one texel per piece of a line between two rings, render 64 x 24; column k is the piece
// from ring k to ring k + 1, row j the line. Chevrons: render 64 x 2, column k at ring k, row
// the chevron's left or right arm.
// `what` *_place: a piece's middle. *_aim: its direction. ring_scale, line_scale, mark_scale:
// `thickness` across, the piece's length along y. wall_scale: its length along the tube, its
// width around it. *_light: its colour.
// @param what choice(ring_place|ring_aim|ring_scale|ring_light|wall_place|wall_aim|wall_scale|line_place|line_aim|line_scale|line_light|mark_place|mark_aim|mark_scale|mark_light) = ring_place -- what this node writes
// @param whole float = 0 0..20000 -- the whole beats travelled (travelled of a chan.beat_grid less its fraction)
// @param phase float = 0 0..1 -- the fraction of the beat travelled
// @param drift float = 0 -100..100 -- where the bars stand against the beats, in bars (drift of a chan.beat_grid)
// @param bass float = 0 0..1 -- the bass (energy_low of a chan.beat_grid): the rings and the chevrons brighten with it
// @param beat float = 0 0..1 -- how far the current beat has gone (beat of a chan.beat_grid): the pulse runs with it
// @param bar float = 0 0..1 -- how far the current bar has gone (bar of a chan.beat_grid): the dashes run with it
// @param bar_index float = 0 0..100000 -- bars counted (bar_index of a chan.beat_grid): the turns of the tube count them
// @param role float = 0 0..4 -- the section's role (section_role of a chan.beat_grid): 3, a drop, turns the tube twice as often
// @param step float = 0.5 0.1..2 -- metres the world moves per beat; the distance between two rings
// @param beats_per_bar float = 4 2..8 -- beats in a bar: the ring on a downbeat is brighter
// @param radius float = 4 1..10 -- the tube's radius, metres
// @param lines int = 18 0..24 -- long lines along the wall
// @param twist float = 1.5 0..5 -- how far the tube rolls with the lane: 1 turns it by the lane's swing over the radius
// @param thickness float = 0.015 0.001..0.1 -- metres across a long line
// @param warmth float = 0 0..1 -- 0 the warm colour, 1 the cold one
// @param roll_up float = 1 0..1 -- 1 the tube, towards 0 it opens out into a flat floor of lines from right to left
// @param roll_bars float = 8 0..64 -- bars from one whole turn of the tube to the next; 0 never turns
// @param roll_length float = 2 0.25..8 -- bars a turn takes
// @param pulse float = 1 0..3 -- the light of the pulse that runs down the tube on every beat, and the dashes on every bar
// @param marks float = 1 0..3 -- the chevrons' light on the tube's bottom
// @param turn_up float = 0 0..1 -- 0 the tube, towards 1 it becomes hoops that turn upright into a tower
// @param storey float = 3 1..10 -- metres between two hoops once the tube has turned up
// @param storey_beats float = 4 1..16 -- beats from one hoop to the next once the tube has turned up
// @param tower_radius float = 6 1..20 -- the hoops' radius once the tube has turned up, metres

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;
const RINGS: u32 = 64u;
const PIECES: u32 = 48u;
// Rings behind the camera: the nearest one stands a few beats behind it.
const BEHIND: f32 = 4.0;
// The wall stands this much outside the rings and lines, so they lie in front of it.
const WALL_OUT: f32 = 0.06;
// The rings' and chevrons' thickness against `thickness`: thin enough that a ring right beside
// the camera still draws as one line.
const THIN: f32 = 0.33;
// Metres the pulse runs in one beat, and a dash in half a bar.
const PULSE_RUN: f32 = 16.0;
const DASH_RUN: f32 = 16.0;
// Half the width of a chevron across the tube's bottom and how far its tip points ahead, metres.
const MARK_HALF: f32 = 0.5;
const MARK_TIP: f32 = 0.45;
const PI: f32 = 3.14159265;
const TAU: f32 = 6.2831853;
const HALF_PI: f32 = 1.5707963;

fn up_share() -> f32 {
    return clamp(params.turn_up, 0.0, 1.0);
}

// What is left of the long lines, the wall, the chevrons and the pulse: they go over the first
// third of the turn up.
fn bare() -> f32 {
    return 1.0 - smoothstep(0.0, 1.0 / 3.0, up_share());
}

// How far the hoops have moved apart and widened.
fn grown() -> f32 {
    return smoothstep(0.2, 0.8, up_share());
}

// How far the tube has turned upright.
fn stood() -> f32 {
    return smoothstep(1.0 / 3.0, 1.0, up_share());
}

// The tube's radius now.
fn radius_now() -> f32 {
    return mix(params.radius, params.tower_radius, grown());
}

// Metres the world moves per beat now.
fn per_beat() -> f32 {
    return mix(params.step, params.storey / max(params.storey_beats, 0.25), grown());
}

// A point of the tube as the turn up places it. The tube lies with its bottom on the floor at
// first. As it widens it sinks, so that it turns about the same line as a lying stage_ascent:
// across the view, `radius` x 0.75 ahead and up.
fn stand(p: vec3f) -> vec3f {
    let pivot = 0.75 * params.radius;
    let q = p + vec3f(0.0, 0.0, 1.5 * (params.radius - radius_now()));
    let a = HALF_PI * stood();
    let d = q - vec3f(0.0, pivot, pivot);
    return vec3f(d.x, pivot + d.y * cos(a) - d.z * sin(a), pivot + d.y * sin(a) + d.z * cos(a));
}

// A direction turned with the tube.
fn stand_dir(d: vec3f) -> vec3f {
    let a = HALF_PI * stood();
    return vec3f(d.x, d.y * cos(a) - d.z * sin(a), d.y * sin(a) + d.z * cos(a));
}

// How much of ring k is left as the rings thin out to one every `storey_beats` beats. The
// ring on such a beat stays; of the others the ones on odd beats go first.
fn hoop_left(k: f32) -> f32 {
    let sb = max(round(params.storey_beats), 1.0);
    let beat = params.whole - BEHIND + k;
    let m = beat - sb * floor(beat / sb);
    if m < 0.5 {
        return 1.0;
    }
    let odd = m - 2.0 * floor(0.5 * m) > 0.5;
    let u = up_share();
    return select(1.0 - smoothstep(0.12, 0.3, u), 1.0 - smoothstep(0.02, 0.18, u), odd);
}

// Metres ahead of the camera of ring k.
fn ring_y(k: f32) -> f32 {
    return (k - BEHIND - params.phase) * per_beat();
}

fn lane_at(y: f32) -> vec4f {
    let u = (y - LANE_NEAR) / (LANE_FAR - LANE_NEAR);
    let l = textureSampleLevel(in0, pn_clamp, vec2f(u, 0.5), 0.0);
    // Unwired, the lane picture is black: a straight lane and a whole tube.
    let wired = textureLoad(in0, vec2i(0, 0), 0).b > 0.0;
    return select(vec4f(0.0, 0.0, 0.0, 1.0), l, wired);
}

// The tube `y` metres ahead: 1 where it stands whole, 0 in the open middle of a gap.
fn tube(y: f32) -> f32 {
    return clamp(lane_at(y).a, 0.0, 1.0);
}

// How much of ring k is left where the tube runs out. The rings on odd beats go first, then
// the one in the middle of the bar, the downbeat's ring last.
fn ring_left(k: f32) -> f32 {
    let t = tube(ring_y(k));
    let bpb = max(params.beats_per_bar, 1.0);
    let beat = params.whole - BEHIND + k;
    let q = beat / bpb + params.drift + 0.5 / bpb;
    let n = i32(floor((q - floor(q)) * bpb));
    if n == 0 {
        return smoothstep(0.03, 0.3, t);
    }
    if n % 2 == 0 {
        return smoothstep(0.35, 0.6, t);
    }
    return smoothstep(0.65, 0.9, t);
}

// How much of the long lines and chevrons is left `y` metres ahead: they go where the tube
// runs out and as it turns up.
fn line_left(y: f32) -> f32 {
    return smoothstep(0.4, 0.9, min(tube(y), bare()));
}

// How wide a slat of the wall is `y` metres ahead, as a share of a whole wall, for the slat
// `s` metres round from the tube's bottom on ring k. Where the tube runs out the wall opens from
// the top down: the roof goes first, the bottom last, so the tube becomes a trough before it
// ends. Each slat has a little draw of its own, so the edge is ragged.
fn wall_left(y: f32, s: f32, k: f32, i: u32) -> f32 {
    let r = max(radius_now(), 0.1);
    let up = clamp(abs(s) / (PI * r), 0.0, 1.0);
    let n = bitcast<u32>(i32(params.whole + k));
    let draw = f32(pn_hash(vec3u(n, i, 17u)).x) / 4294967296.0;
    let edge = mix(0.05, 0.5, up) + 0.12 * (draw - 0.5);
    return smoothstep(edge, edge + 0.4, min(tube(y), bare()));
}

// How far the sheet is rolled up: 0 flat, 1 the tube. The plane's floor gives way first.
fn curl() -> f32 {
    return smoothstep(0.15, 0.95, clamp(params.roll_up, 0.0, 1.0));
}

// What only the finished tube has: the long lines, the wall and the chevrons.
fn closing() -> f32 {
    return smoothstep(0.85, 1.0, curl());
}

// The turn of the whole tube now, radians: a whole turn every `roll_bars` bars.
fn turning() -> f32 {
    if params.roll_bars <= 0.0 {
        return 0.0;
    }
    let span = max(params.roll_length, 0.25);
    var every = max(params.roll_bars, span);
    if abs(params.role - 3.0) < 0.5 {
        every = max(0.5 * every, span);
    }
    let bars = params.bar_index + clamp(params.bar, 0.0, 1.0);
    let i = floor(bars / every);
    let p = clamp((bars - i * every) / span, 0.0, 1.0);
    let way = select(-1.0, 1.0, i - 2.0 * floor(0.5 * i) < 0.5);
    return way * TAU * p * p * (3.0 - 2.0 * p);
}

// The roll of the long lines at ring k, radians: with the lane, and the whole tube's turn.
fn roll(k: f32) -> f32 {
    let r = max(radius_now(), 0.1);
    return -params.twist * lane_at(ring_y(k)).r / r + turning();
}

// A point of the sheet: `s` metres around from the tube's bottom, `y` ahead, `out` metres
// outside the sheet. Rolled up, s = radius x angle.
fn sheet(s: f32, y: f32, out: f32) -> vec3f {
    let r = max(radius_now(), 0.1);
    let bend = curl() / r;
    if bend < 0.0001 {
        return vec3f(s, y, -out);
    }
    let a = bend * s;
    let far = 1.0 / bend + out;
    return vec3f(far * sin(a), y, 1.0 / bend - far * cos(a));
}

// An angle around the tube as the place on the sheet: from -pi to pi times the radius, so the
// sheet's two edges meet at the top.
fn around(a: f32) -> f32 {
    let w = a - TAU * floor((a + PI) / TAU);
    return w * radius_now();
}

// Where line j meets ring k.
fn on_wall(k: f32, j: f32) -> vec3f {
    let n = f32(clamp(params.lines, 1, 24));
    return sheet(around(TAU * j / n + roll(k)), ring_y(k), 0.0);
}

fn warm_cold(warm: vec3f, cold: vec3f) -> vec3f {
    return mix(warm, cold, clamp(params.warmth, 0.0, 1.0));
}

// Brighter on a downbeat: the beat of ring k starts a bar.
fn downbeat(k: f32) -> f32 {
    let bpb = max(params.beats_per_bar, 1.0);
    let beat = params.whole - BEHIND + k;
    let q = beat / bpb + params.drift + 0.5 / bpb;
    return select(1.0, 1.4, q - floor(q) < 1.0 / bpb);
}

// The pulse at y metres ahead: a band of light running from the camera ahead in each beat,
// fading as it goes.
fn pulse_at(y: f32) -> f32 {
    let b = clamp(params.beat, 0.0, 1.0);
    let d = (y - b * PULSE_RUN) / 1.2;
    return params.pulse * bare() * exp(-d * d) * (1.0 - b);
}

// The dash on a line at y metres ahead: a bright stretch running ahead, two in each bar.
fn dash_at(y: f32) -> f32 {
    let half = fract(2.0 * clamp(params.bar, 0.0, 1.0));
    let d = (y - (half * DASH_RUN - 1.0)) / 1.5;
    return params.pulse * 2.5 * exp(-d * d) * (1.0 - 0.5 * half);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let cell = vec2u(v.pos.xy);
    if cell.x >= RINGS || cell.y >= PIECES {
        return vec4f(0.0, 0.0, 0.0, 1.0);
    }
    let k = f32(cell.x);
    let r = max(radius_now(), 0.1);
    let bass = clamp(params.bass, 0.0, 1.0);
    let w = params.what;
    let width = TAU * r / f32(PIECES);
    let i = f32(cell.y);

    if w == what_ring_place || w == what_ring_aim || w == what_ring_scale || w == what_ring_light {
        let y = ring_y(k);
        // The pulse narrows the rings it passes.
        let squeeze = -0.1 * pulse_at(y) * closing();
        let s0 = -PI * r + i * width;
        let a = sheet(s0, y, squeeze);
        let b = sheet(s0 + width, y, squeeze);
        if w == what_ring_place {
            return vec4f(stand(0.5 * (a + b)), 1.0);
        }
        if w == what_ring_aim {
            return vec4f(stand_dir(b - a), 1.0);
        }
        // A ring thins out where the tube runs out and as it turns up.
        let here = ring_left(k) * hoop_left(k);
        if w == what_ring_scale {
            let on = here >= 0.03;
            let t = select(0.0, THIN * params.thickness, on);
            return vec4f(t, select(0.0, length(b - a), on), t, 1.0);
        }
        // White as the plane's floor while flat, the tube's colour rolled up.
        let tint = warm_cold(vec3f(1.0, 0.62, 0.32), vec3f(0.55, 0.8, 1.0));
        // Every hoop left lit as a downbeat's ring, as stage_ascent's hoops are.
        let down = mix(downbeat(k), 1.4, grown());
        let lit = tint * (0.35 + 0.35 * bass) * down * (1.0 + 2.5 * pulse_at(y));
        let c = mix(vec3f(1.0), lit, smoothstep(0.3, 1.0, curl()));
        return vec4f(c * here, 1.0);
    }
    if w == what_wall_place || w == what_wall_aim || w == what_wall_scale {
        let y = ring_y(k) + 0.5 * per_beat();
        let s = -PI * r + (i + 0.5) * width;
        if w == what_wall_place {
            return vec4f(stand(sheet(s, y, WALL_OUT)), 1.0);
        }
        if w == what_wall_aim {
            return vec4f(stand_dir(sheet(s + 0.01, y, WALL_OUT) - sheet(s - 0.01, y, WALL_OUT)), 1.0);
        }
        // The wall opens from the top down where the tube runs out, its slats narrowing, and
        // closes in slats as the sheet rolls up.
        let here = wall_left(y, s, k, cell.y);
        let shut = closing();
        let on = here >= 0.02 && shut > 0.01;
        let across = width * (r + WALL_OUT) / r * 1.04 * here;
        return vec4f(select(0.0, per_beat() * shut, on), select(0.0, across, on), 1.0, 1.0);
    }
    if w == what_mark_place || w == what_mark_aim || w == what_mark_scale || w == what_mark_light {
        // A chevron every other ring, fixed to the world, its tip pointing ahead.
        let even = (params.whole + k) - 2.0 * floor(0.5 * (params.whole + k)) < 0.5;
        let side = select(-1.0, 1.0, cell.y == 1u);
        let y = ring_y(k) + 0.5 * per_beat();
        let turn = around(turning());
        let a = sheet(turn + side * MARK_HALF, y - 0.5 * MARK_TIP, -0.01);
        let b = sheet(turn, y + 0.5 * MARK_TIP, -0.01);
        if w == what_mark_place {
            return vec4f(stand(0.5 * (a + b)), 1.0);
        }
        if w == what_mark_aim {
            return vec4f(stand_dir(b - a), 1.0);
        }
        let shut = closing();
        if w == what_mark_scale {
            let on = even && cell.y < 2u && line_left(y) >= 0.03 && shut > 0.02 && params.marks > 0.0;
            let t = select(0.0, THIN * params.thickness, on);
            return vec4f(t, select(0.0, length(b - a), on), t, 1.0);
        }
        let c = warm_cold(vec3f(1.0, 0.3, 0.6), vec3f(0.3, 0.9, 1.0));
        return vec4f(c * params.marks * (0.25 + 1.1 * bass) * shut * line_left(y), 1.0);
    }

    // The long lines.
    if cell.y >= 24u {
        return vec4f(0.0, 0.0, 0.0, 1.0);
    }
    let j = f32(cell.y);
    let shown = i32(cell.y) < params.lines;
    let a = on_wall(k, j);
    let b = on_wall(k + 1.0, j);
    if w == what_line_place {
        return vec4f(stand(0.5 * (a + b)), 1.0);
    }
    if w == what_line_aim {
        return vec4f(stand_dir(b - a), 1.0);
    }
    let here = line_left(0.5 * (a.y + b.y));
    let shut = closing();
    if w == what_line_scale {
        // A line crossing the sheet's open edge would span it while the sheet is not closed.
        let on = shown && here >= 0.03 && shut > 0.02 && length(b - a) < 2.0 * per_beat() + 1.0;
        let t = select(0.0, params.thickness, on);
        return vec4f(t, select(0.0, length(b - a), on), t, 1.0);
    }
    let c = warm_cold(vec3f(0.85, 0.42, 0.62), vec3f(0.45, 0.55, 1.0));
    let third = cell.y % 3u == 0u;
    let dash = select(0.0, dash_at(0.5 * (a.y + b.y)), third);
    return vec4f(c * here * shut * (0.5 + 0.25 * bass + dash), 1.0);
}
