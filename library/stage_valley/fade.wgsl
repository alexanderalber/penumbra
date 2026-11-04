// The render faded with distance, so the lines melt into the sky towards the horizon, and cut at
// the water: what lies below the water line (a terrace sinking, its foot) is left out, so the lake
// shows there. mirror.wgsl does the same for the lake's mirror of what stands on the water.
// in0: the render's picture, in1: its data output (the distance from the camera in alpha, 0
// where nothing was drawn). The data pass leaves out a faint haze (under 5 % alpha), which
// fades itself: a pixel with no distance in or around it and under 5 % alpha keeps its colour.
// The cut needs the stage's camera: eye, pitch, fov, roll and side as the lake has them.
// @param start float = 12 0..50 -- metres where the fade begins
// @param end float = 40 0..100 -- metres where nothing is left
// @param eye float = 1.2 0..10 -- the stage camera's height over the water, metres
// @param pitch float = -1 -60..60 -- the stage camera's tilt up, degrees
// @param fov float = 58 10..120 -- the stage camera's vertical field of view, degrees
// @param roll float = 0 -45..45 -- the stage camera's roll, degrees
// @param side float = 0 -45..45 -- the stage camera's turn to the right, degrees

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let last = vec2i(textureDimensions(in0)) - 1;
    let at = clamp(vec2i(v.pos.xy), vec2i(0), last);
    let c = textureLoad(in0, at, 0);
    // The data pass has no antialiasing, so the soft edge of a line can fall on a pixel it
    // left empty: there the nearest distance around stands in.
    var d = textureLoad(in1, at, 0).a;
    if d <= 0.0 {
        d = 1e9;
        for (var dy = -1; dy <= 1; dy++) {
            for (var dx = -1; dx <= 1; dx++) {
                let n = textureLoad(in1, clamp(at + vec2i(dx, dy), vec2i(0), last), 0).a;
                if n > 0.0 {
                    d = min(d, n);
                }
            }
        }
        if d > 1e8 && c.a < 0.05 {
            return c;
        }
    }

    // The ray of the stage's camera through this pixel of its picture, its forward part 1, so the
    // point lies `d` along it.
    let res = pn.resolution;
    let t = tan(radians(0.5 * clamp(params.fov, 1.0, 170.0)));
    let q = vec2f((v.pos.x - 0.5 * res.x) / res.y, 0.5 - v.pos.y / res.y) * 2.0 * t;
    let r = radians(-params.roll);
    let p = vec2f(cos(r) * q.x - sin(r) * q.y, sin(r) * q.x + cos(r) * q.y);
    let ahead = normalize(vec3f(tan(radians(params.side)), 1.0, tan(radians(params.pitch))));
    let right = normalize(vec3f(ahead.y, -ahead.x, 0.0));
    let up = cross(right, ahead);
    let ray = ahead + p.x * right + p.y * up;
    let height = max(params.eye, 0.0) + min(d, 1e4) * ray.z;
    let above = smoothstep(-0.02, 0.02, height);
    return c * above * (1.0 - smoothstep(params.start, params.end, d));
}
