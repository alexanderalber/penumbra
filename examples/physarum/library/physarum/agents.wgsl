// One step of every agent: smell at three points ahead, turn towards the strongest, move.
//
// State of an agent, one texel, 32-bit floats (format 32f):
//   r: x, g: y, in trail cells from the top left; b: heading in turns (0..1); a: 1 alive, -1 dead.
//
// in0: the agents (the tex.feedback). in1: the trail of the last frame; its size is the
//      simulation size. in2: fields.wgsl at the simulation size: r what the inputs add to the
//      smell, g 1 where the agents may not go, a where agents are born when born is food.
//
// @param speed float = 1 0.1..4 -- step length as a share of step_length; 2 moves twice as far per frame
// @param pause bool = false -- holds the agents where they are
// @param sensor_angle float = 22.5 1..90 -- degrees between the middle sensor and each side sensor
// @param sensor_distance float = 9 1..40 -- how far ahead the sensors are, in cells
// @param turn_angle float = 90 1..90 -- degrees an agent turns per step towards the stronger side
// @param step_length float = 1 0.1..4 -- cells an agent moves per step
// @param jitter float = 0 0..45 -- random wobble per step, in degrees
// @param renew float = 0 0..0.05 -- chance per step that an agent dies and is born again
// @param born choice(anywhere|food) = anywhere -- where a new agent appears; food: only where the food input is bright (1 in 200 anywhere, so the agents live on without food)
// @param edges choice(wrap|bounce) = wrap -- wrap: agents leaving one side come back on the other; bounce: they turn back

const TAU: f32 = 6.2831853;

fn rand4(p: vec2i, salt: u32) -> vec4f {
    let h = pn_hash(vec3u(vec2u(p), u32(pn_time.frame) * 4u + salt));
    let h2 = pn_hash(h);
    return vec4f(vec4u(h.x, h.y, h.z, h2.x) >> vec4u(8u)) / 16777216.0;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = vec2i(v.pos.xy);
    let s = textureLoad(in0, p, 0);
    if params.pause != 0u {
        return s;
    }
    let size = vec2f(textureDimensions(in1));
    let sar = params.sensor_angle / 360.0 * TAU;
    let sd = params.sensor_distance;
    let rar = params.turn_angle / 360.0 * TAU;
    let sl = params.step_length * params.speed;
    let r = rand4(p, 0u);

    var pos = s.rg;
    var ang = s.b * TAU;
    if s.a < 0.0 || r.x < params.renew || blocked(pos, size) {
        // Born again: a few random places, the first one that suits. None: dead until next frame.
        // Born on food, 1 in 200 tries a place anywhere, so the agents live on without a food input.
        let anywhere = params.born == born_anywhere || r.w < 0.005;
        for (var i = 0u; i < 16u; i++) {
            let q = rand4(p, 1u + i);
            let at = q.xy * size;
            let f = textureLoad(in2, vec2i(at), 0);
            if f.g < 0.5 && (anywhere || f.a > 0.05) {
                return vec4f(at, q.z, 1.0);
            }
        }
        return vec4f(pos, s.b, -1.0);
    }
    let f = smelled(pos, ang, sd, size);
    let fl = smelled(pos, ang - sar, sd, size);
    let fr = smelled(pos, ang + sar, sd, size);
    if f < fl && f < fr {
        ang = ang + select(-rar, rar, r.y < 0.5);
    } else if fl > f && fl > fr {
        ang = ang - rar;
    } else if fr > f && fr > fl {
        ang = ang + rar;
    }
    ang = ang + (r.z - 0.5) * 2.0 * params.jitter / 360.0 * TAU;
    var next = pos + sl * vec2f(cos(ang), sin(ang));
    if params.edges == edges_bounce {
        if next.x < 0.0 || next.x >= size.x {
            ang = 0.5 * TAU - ang;
            next.x = clamp(next.x, 0.0, size.x - 0.01);
        }
        if next.y < 0.0 || next.y >= size.y {
            ang = -ang;
            next.y = clamp(next.y, 0.0, size.y - 0.01);
        }
    }
    next = next - size * floor(next / size);
    if blocked(next, size) {
        // A wall ahead: turn to a random heading and stay.
        ang = r.w * TAU;
    } else {
        pos = next;
    }
    return vec4f(pos, fract(ang / TAU), 1.0);
}

fn blocked(pos: vec2f, size: vec2f) -> bool {
    let q = clamp(vec2i(pos), vec2i(0), vec2i(size) - vec2i(1));
    return textureLoad(in2, q, 0).g > 0.5;
}

// What a sensor at distance d in direction a smells: trail (rgb drawn, alpha the far smell of
// food) plus what the inputs add. A wall smells of nothing.
fn smelled(pos: vec2f, a: f32, d: f32, size: vec2f) -> f32 {
    let uv = (pos + d * vec2f(cos(a), sin(a))) / size;
    let t = textureSampleLevel(in1, pn_repeat, uv, 0.0);
    let f = textureSampleLevel(in2, pn_repeat, uv, 0.0);
    return select(t.r + t.g + t.b + t.a + f.r, -1e4, f.g > 0.5);
}
