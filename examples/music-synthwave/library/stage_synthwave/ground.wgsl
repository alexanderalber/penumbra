// The ground under the horizon: darkens the sky picture in in0 below the line where the floor
// fades out, so the floor stands on black there instead of on the nebula. Where a sky element
// is cut hard at the horizon (the sun), the ground then fits it, with or without the floor's
// lines and in a gap of the floor. `ground` 0 leaves the picture as it is.
// @param ground float = 0 0..1 -- how dark the sky below the horizon gets: 0 not at all, 1 black
// @param soft float = 0.012 0..0.5 -- picture heights over which the darkness sets in below the horizon
// @param eye float = 1.5 0..10 -- the camera's height over the floor, metres (height of the stage's pose)
// @param pitch float = -2 -60..60 -- the camera's tilt up, degrees (pitch of the stage's pose)
// @param fov float = 60 10..120 -- the camera's vertical field of view, degrees (fov of the stage's pose)
// @param roll float = 0 -45..45 -- the camera's roll, degrees (roll of the stage's pose)
// @param side float = 0 -45..45 -- the camera's turn to the right, degrees (side of the stage's pose)
// @param reach float = 10 1..100 -- metres ahead where the floor fades out, the fade's `end`: the horizon lies there

// How far this pixel lies above the horizon, in picture heights of a 60 degree view (the sky
// elements' sky_at() helper, y only).
fn above(px: vec2f) -> f32 {
    let res = pn.resolution;
    let t = tan(radians(0.5 * clamp(params.fov, 1.0, 170.0)));
    let q = vec2f((px.x - 0.5 * res.x) / res.y, 0.5 - px.y / res.y) * 2.0 * t;
    let r = radians(-params.roll);
    let y = sin(r) * q.x + cos(r) * q.y;
    let down = atan(max(params.eye, 0.0) / max(params.reach, 0.1)) + radians(params.pitch);
    return (y + tan(down)) / (2.0 * tan(radians(30.0)));
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let c = textureLoad(in0, vec2i(v.pos.xy), 0);
    let g = clamp(params.ground, 0.0, 1.0);
    if g <= 0.0 {
        return c;
    }
    let below = smoothstep(0.0, max(params.soft, 1e-4), -above(v.pos.xy));
    return c * (1.0 - g * below);
}
