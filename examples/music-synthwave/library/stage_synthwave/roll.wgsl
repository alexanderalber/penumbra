// The floor's lines from right to left as a sheet that rolls up into a tube, for
// geo.tex_instance with segment.obj (a thin prism one unit long along y): the plane's half of the
// roll-up into stage_tunnel. At `roll_up` 0 nothing is drawn and the floor grid stands as it is.
// As `roll_up` grows the grid gives way to these lines, which stand where its lines from right
// to left stand, and then the lines bend up on both sides into rings of `radius` around an axis
// `radius` metres up. stage_tunnel's rings do the same with the same `roll_up`, so the two
// pictures show the same lines while a switch fades between them.
// A ring stands on every beat of the distance travelled: ring k on beat `whole` - 4 + k, from 2
// metres behind the camera on. Render it 64 x 48: column k the ring, row i its piece i of 48.
// `what` place: a piece's middle. aim: its direction. scale: `thickness` across, its length along
// y. light: its colour.
// @param what choice(place|aim|scale|light) = place -- what this node writes
// @param phase float = 0 0..1 -- the fraction of the beat travelled (travelled of a chan.beat_grid modulo 1)
// @param step float = 0.5 0.1..2 -- metres the world moves per beat; the distance between two lines
// @param radius float = 4 1..10 -- the rings' radius once rolled up, metres
// @param roll_up float = 0 0..1 -- 0 the floor grid, 1 rolled up into rings
// @param thickness float = 0.005 0.001..0.1 -- metres across a line

const RINGS: u32 = 64u;
const PIECES: u32 = 48u;
const BEHIND: f32 = 4.0;
const PI: f32 = 3.14159265;
const TAU: f32 = 6.2831853;

// How far the sheet is rolled up: the same as stage_tunnel's.
fn curl() -> f32 {
    return smoothstep(0.15, 0.95, clamp(params.roll_up, 0.0, 1.0));
}

// A point of the sheet `s` metres from its middle across and `y` ahead.
fn sheet(s: f32, y: f32) -> vec3f {
    let r = max(params.radius, 0.1);
    let bend = curl() / r;
    if bend < 0.0001 {
        return vec3f(s, y, 0.0);
    }
    let a = bend * s;
    return vec3f(sin(a) / bend, y, (1.0 - cos(a)) / bend);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let cell = vec2u(v.pos.xy);
    if cell.x >= RINGS || cell.y >= PIECES {
        return vec4f(0.0, 0.0, 0.0, 1.0);
    }
    let r = max(params.radius, 0.1);
    let width = TAU * r / f32(PIECES);
    let y = (f32(cell.x) - BEHIND - params.phase) * params.step;
    let s0 = -PI * r + f32(cell.y) * width;
    let a = sheet(s0, y);
    let b = sheet(s0 + width, y);
    if params.what == what_place {
        return vec4f(0.5 * (a + b), 1.0);
    }
    if params.what == what_aim {
        return vec4f(b - a, 1.0);
    }
    // The lines take over from the grid over the first fifth of the roll.
    let take = smoothstep(0.0, 0.2, clamp(params.roll_up, 0.0, 1.0));
    if params.what == what_scale {
        let on = take > 0.01;
        let t = select(0.0, params.thickness, on);
        return vec4f(t, select(0.0, length(b - a), on), t, 1.0);
    }
    return vec4f(vec3f(take), 1.0);
}
