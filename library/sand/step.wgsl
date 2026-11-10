// Falling sand: one step per pass. The node's `iterations` (under "more") is the number of
// steps per frame; each pass reads the pass before as in0.
//
// A block cellular automaton (Margolus neighbourhood). The grid is cut into 2 x 2 blocks; the
// cut moves by one cell on every other step. Each block rearranges its own four cells: a
// grain falls into the empty cell below it, or slides down a diagonal, or rolls sideways for a
// few steps after landing. A rearrangement only swaps cells, so no grain is ever lost or
// copied. "Below" is one of the four grid directions, chosen per block and step at random in
// proportion to the gravity vector, so any angle works.
//
// State texture: r = 1 where a grain is, 0 where none; g = the grain's colour (0..1, a
// position in the display's palette, or a hue); b = the grain's own variation (0..1, also
// decides which grains are sticky); a = 1 + steps the grain may still roll. All of it is exact
// in half floats, so the state stays in the default 16-bit format.
//
// in0: the state (from the tex.feedback; from the second pass on, the pass before).
// in1: walls (at the simulation size, 0 or 1): where it is bright, no grain can go.
// in2: sources (at the simulation size): where it is bright, new grains appear. Strength is
//      the brightest of r, g, b.
// in3: the drain input, any size: where it is bright, grains are removed.
//
// @param gravity float = 0 -180..180 -- the direction the sand falls, in degrees; 0 down, 90 right, 180 up, -90 left; bind it to tilt the world
// @param amount float = 1 0..4 -- how much sand pours: pour times this; bind it to a level to pour with the music
// @param fall float = 0.33 0..1 -- chance per step that a grain with nothing below falls; below 1 slows the fall against the flow of the piles
// @param slide float = 0.8 0..1 -- chance per step that a grain on a slope slides down it; lower makes flowing piles rougher and slower to settle
// @param roll int = 2 0..30 -- steps a grain may still roll sideways over flat ground after it landed; spreads landing grains a little, does not change the 45 degree slopes
// @param sticky float = 0.03 0..1 -- share of grains that slide only rarely (a twentieth as often) and never roll; gives rougher piles and an occasional jam
// @param pause bool = false -- holds the state
// @param in_wall choice(rise|freeze|delete) = rise -- grains inside a wall that appeared over them; rise: they are pushed out, mostly against gravity; freeze: they stay, hidden, until the wall leaves; delete: they vanish
// @param colors choice(time|source) = time -- colour of new grains; time: moves along the display's palette and back; source: the hue of the source picture
// @param cycle float = 0.05 0..1 -- time: trips along the palette and back per second
// @param jitter float = 0.1 0..0.5 -- random spread of a new grain's colour
// @param edges choice(closed|open|wrap) = open -- closed: the edges are walls; open: grains that fall out of the picture are gone; wrap: what leaves one side comes in at the other (needs even width and height)
// @param pour float = 0.0015 0..0.2 -- chance per step that a fully bright source cell makes a new grain, times amount
// @param drain float = 0.0 0..1 -- chance per step that a grain on a fully bright cell of the drain input is removed

const DOWN = 0;
const UP = 1;
const RIGHT = 2;
const LEFT = 3;

// Offset inside the block of local cell li (0 = top left, 1 = top right, 2 = bottom left,
// 3 = bottom right, with "bottom" in the direction dir).
fn local_off(dir: i32, li: i32) -> vec2i {
    let c = li & 1;
    let row = li >> 1;
    var o = vec2i(c, row);
    if dir == UP {
        o = vec2i(c, 1 - row);
    } else if dir == RIGHT {
        o = vec2i(row, c);
    } else if dir == LEFT {
        o = vec2i(1 - row, c);
    }
    return o;
}

fn unit(h: u32) -> f32 {
    return f32(h >> 8u) / 16777216.0;
}

fn is_wall(gp: vec2i, size: vec2i) -> bool {
    let wsize = vec2i(textureDimensions(in1));
    let wp = clamp((gp * wsize) / size, vec2i(0), wsize - 1);
    let w = textureLoad(in1, wp, 0);
    return w.r > 0.5;
}

fn opposite(d: i32) -> i32 {
    var r = UP;
    if d == UP {
        r = DOWN;
    } else if d == RIGHT {
        r = LEFT;
    } else if d == LEFT {
        r = RIGHT;
    }
    return r;
}

// A direction perpendicular to d: s 0 or 1 picks which.
fn side(d: i32, s: bool) -> i32 {
    var r = RIGHT;
    if d == RIGHT || d == LEFT {
        r = DOWN;
        if s {
            r = UP;
        }
    } else if s {
        r = LEFT;
    }
    return r;
}

fn hue(c: vec3f) -> f32 {
    let mx = max(c.r, max(c.g, c.b));
    let mn = min(c.r, min(c.g, c.b));
    let d = mx - mn;
    var h = 0.0;
    if d > 1e-5 {
        if mx == c.r {
            h = (c.g - c.b) / d;
        } else if mx == c.g {
            h = 2.0 + (c.b - c.r) / d;
        } else {
            h = 4.0 + (c.r - c.g) / d;
        }
    }
    return fract(h / 6.0 + 1.0);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let size = vec2i(textureDimensions(in0));
    let p = vec2i(v.pos.xy);
    let me = textureLoad(in0, p, 0);
    if params.pause != 0u {
        return me;
    }
    let pour = params.pour * params.amount;
    let edges = params.edges;
    let drain = params.drain;
    let step = u32(pn_time.frame) * u32(max(pn.iterations, 1)) + u32(pn.iteration);
    let ang = radians(params.gravity);
    let gx = sin(ang);
    let gy = cos(ang);
    // The block cut moves by one cell on every step along the main axis of gravity, so a
    // falling grain is at the top of its block on every step. Across, it follows 0, 1, 1, 0:
    // with a plain diagonal shift a grain could only ever slide one way (by its parity).
    let along = i32(step & 1u);
    let across = i32(((step >> 1u) ^ step) & 1u);
    var o = vec2i(across, along);
    if abs(gx) > abs(gy) {
        o = vec2i(along, across);
    }
    var org = ((p + 2 - o) / 2) * 2 - (2 - o);
    if edges == edges_wrap {
        if org.x < 0 {
            org.x += size.x;
        }
        if org.y < 0 {
            org.y += size.y;
        }
    }
    let h = pn_hash(vec3u(u32(org.x + 2), u32(org.y + 2), step));

    // The four cells of the block, in grid order: (0,0), (1,0), (0,1), (1,1).
    var st: array<vec4f, 4>;
    var wl: array<bool, 4>;
    var ins: array<bool, 4>;
    var any_buried = false;
    for (var k = 0; k < 4; k++) {
        var gp = org + vec2i(k & 1, k >> 1);
        var inside = all(gp >= vec2i(0)) && all(gp < size);
        if edges == edges_wrap {
            gp = (gp + size) % size;
            inside = true;
        }
        ins[k] = inside;
        if inside {
            st[k] = textureLoad(in0, gp, 0);
            wl[k] = is_wall(gp, size);
        } else {
            st[k] = vec4f(0.0, 0.0, 0.0, 1.0);
            wl[k] = edges == edges_closed;
        }
        if inside && wl[k] && st[k].r > 0.5 {
            any_buried = true;
        }
    }
    let rise = any_buried && params.in_wall == in_wall_rise;

    // "Down" for this block: a grid direction, picked in proportion to the gravity vector.
    let u0 = unit(h.x);
    var dir = DOWN;
    if u0 * (abs(gx) + abs(gy)) < abs(gx) {
        dir = select(LEFT, RIGHT, gx > 0.0);
    } else {
        dir = select(UP, DOWN, gy > 0.0);
    }
    if rise {
        // Buried grains wander out of the wall, mostly against gravity.
        let u = unit(h.y);
        if u < 0.6 {
            dir = opposite(dir);
        } else if u < 0.9 {
            dir = side(dir, u < 0.75);
        }
    }

    // Kinds in the local frame: 0 free (a grain may move in), 1 a grain that moves, 2 fixed.
    // Cells beyond an open edge are free: a grain that moves there is gone.
    var kd: array<i32, 4>;
    var ls: array<vec4f, 4>;
    for (var li = 0; li < 4; li++) {
        let lo = local_off(dir, li);
        let k = lo.x + 2 * lo.y;
        let grain = st[k].r > 0.5;
        ls[li] = st[k];
        var kind = 0;
        if rise {
            if !ins[k] {
                kind = select(2, 0, edges == edges_open);
            } else if grain && wl[k] {
                kind = 1;
            } else if grain {
                kind = 2;
            }
        } else {
            if wl[k] {
                kind = 2;
            } else if grain {
                kind = 1;
            }
        }
        kd[li] = kind;
    }

    // Rearrange. s[i] is the local cell whose content ends up in local cell i.
    var k4 = vec4i(kd[0], kd[1], kd[2], kd[3]);
    var s = vec4i(0, 1, 2, 3);
    var fell = vec4<bool>(false, false, false, false);
    // Fall.
    let h2 = pn_hash(h);
    let fall_ok = unit(h2.y) < params.fall || rise;
    if fall_ok && k4.x == 1 && k4.z == 0 {
        s = vec4i(s.z, s.y, s.x, s.w);
        k4 = vec4i(0, k4.y, 1, k4.w);
        fell.z = true;
    }
    if fall_ok && k4.y == 1 && k4.w == 0 {
        s = vec4i(s.x, s.w, s.z, s.y);
        k4 = vec4i(k4.x, 0, k4.z, 1);
        fell.w = true;
    }
    // Slide down a diagonal: the cell beside and the cell diagonally below are free.
    let slide_ok = unit(h.z) < params.slide || rise;
    let sticky0 = ls[s.x].b < params.sticky && !rise && unit(h.z) > params.slide * 0.05;
    let sticky1 = ls[s.y].b < params.sticky && !rise && unit(h.z) > params.slide * 0.05;
    if slide_ok {
        if k4.x == 1 && k4.z != 0 && k4.y == 0 && k4.w == 0 && !sticky0 {
            s = vec4i(s.w, s.y, s.z, s.x);
            k4 = vec4i(0, k4.y, k4.z, 1);
            fell.w = true;
        } else if k4.y == 1 && k4.w != 0 && k4.x == 0 && k4.z == 0 && !sticky1 {
            s = vec4i(s.x, s.z, s.y, s.w);
            k4 = vec4i(k4.x, 0, 1, k4.w);
            fell.z = true;
        }
    }
    // Roll sideways over flat ground, for a few steps after landing.
    let roll_ok = unit(h2.x) < 0.7 && !rise;
    if roll_ok {
        if k4.x == 1 && k4.z != 0 && k4.w != 0 && k4.y == 0 && !sticky0 && ls[s.x].a > 1.5 && !fell.x {
            s = vec4i(s.y, s.x, s.z, s.w);
            k4 = vec4i(0, 1, k4.z, k4.w);
        } else if k4.y == 1 && k4.w != 0 && k4.z != 0 && k4.x == 0 && !sticky1 && ls[s.y].a > 1.5 && !fell.y {
            s = vec4i(s.y, s.x, s.z, s.w);
            k4 = vec4i(1, 0, k4.z, k4.w);
        }
    }

    // My cell: which local cell am I, and whose content do I get?
    let mo = p - org;
    let mo2 = vec2i(((mo.x % 2) + 2) % 2, ((mo.y % 2) + 2) % 2);
    var row = mo2.y;
    var col = mo2.x;
    if dir == UP {
        row = 1 - mo2.y;
    } else if dir == RIGHT {
        row = mo2.x;
        col = mo2.y;
    } else if dir == LEFT {
        row = 1 - mo2.x;
        col = mo2.y;
    }
    let mine = row * 2 + col;
    var src = s.x;
    var did_fall = fell.x;
    if mine == 1 {
        src = s.y;
        did_fall = fell.y;
    } else if mine == 2 {
        src = s.z;
        did_fall = fell.z;
    } else if mine == 3 {
        src = s.w;
        did_fall = fell.w;
    }
    var out = ls[src];
    if out.r > 0.5 {
        var e = max(out.a - 2.0, 0.0);
        if did_fall {
            e = f32(params.roll);
        }
        out = vec4f(1.0, out.g, out.b, 1.0 + e);
    } else {
        out = vec4f(0.0, 0.0, 0.0, 1.0);
    }

    // Per cell: delete buried grains, drain, pour.
    let ch = pn_hash(vec3u(u32(p.x), u32(p.y), step ^ 0x9e3779b9u));
    let wall_here = is_wall(p, size);
    if out.r > 0.5 && wall_here && params.in_wall == in_wall_delete {
        out = vec4f(0.0, 0.0, 0.0, 1.0);
    }
    if out.r > 0.5 && drain > 0.0 {
        let dsize = vec2i(textureDimensions(in3));
        let d = textureLoad(in3, clamp((p * dsize) / size, vec2i(0), dsize - 1), 0);
        if unit(ch.x) < drain * max(d.r, max(d.g, d.b)) {
            out = vec4f(0.0, 0.0, 0.0, 1.0);
        }
    }
    if out.r < 0.5 && !wall_here && pour > 0.0 {
        let ssize = vec2i(textureDimensions(in2));
        let sc = textureLoad(in2, clamp((p * ssize) / size, vec2i(0), ssize - 1), 0);
        let strength = max(sc.r, max(sc.g, sc.b));
        if unit(ch.y) < pour * strength {
            var t = 0.5 - 0.5 * cos(6.2831853 * pn_time.seconds * params.cycle);
            if params.colors == colors_source {
                t = hue(sc.rgb);
            }
            t += (unit(ch.z) - 0.5) * 2.0 * params.jitter;
            if params.colors == colors_source {
                t = fract(t + 1.0);
            } else {
                t = clamp(t, 0.0, 1.0);
            }
            out = vec4f(1.0, t, unit(pn_hash(ch).x), 1.0);
        }
    }
    return out;
}
