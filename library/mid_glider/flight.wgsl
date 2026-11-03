// Where the glider flies and how big it is drawn. Render it 1 x 1 and read it with
// chan.from_tex: red y (metres ahead), green z (metres up), blue the scale.
// At `presence` 1 it flies `at` metres ahead, `hover` metres up. Towards 0 it climbs away ahead
// of the camera, up to 6 m further and 1 m higher, and shrinks over the last quarter, so it
// leaves without a cut. At 0 it is not drawn at all. The component smooths `presence` before
// it arrives here, so the glider glides in and out over a few seconds.
// @param at float = 3 -8..56 -- where the glider flies, metres ahead of the camera
// @param presence float = 1 0..1 -- 1 the glider flies at `at`, towards 0 it climbs away ahead, 0 none
// @param hover float = 1.2 0..3.5 -- metres above the floor (the camera is 2 m up, the ceiling 4 m)
// @param bob float = 0 -1..1 -- metres up or down on top of `hover`; bind it to a slow wave
// @param scale float = 0.16 0..1 -- the glider's size; 0.16 makes it 2.4 m long

// Metres the glider flies ahead of `at` and climbs at presence 0.
const AWAY: f32 = 6.0;
const CLIMB: f32 = 1.0;

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = clamp(params.presence, 0.0, 1.0);
    let a = 1.0 - p;
    let e = a * a * (3.0 - 2.0 * a);
    let y = params.at + AWAY * e;
    let z = params.hover + params.bob + CLIMB * e;
    let scale = select(0.0, params.scale * smoothstep(0.0, 0.25, p), p > 0.001);
    return vec4f(y, z, scale, 1.0);
}
