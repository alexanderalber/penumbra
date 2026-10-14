// The positions step. in0: last frame's positions (rgb, life in a), in1: last frame's
// velocities, in2: the start positions. A spent particle starts again where in2 says, with
// life 1.
//
// Textures hold half floats, and the life lost per frame is below one half-float step near 1,
// so the step is dithered per texel and frame to keep its mean.
// @param speed float = 0.6 0..2 -- how fast the whole simulation runs
// @param life_damping float = 0.001 0..0.01 -- life lost per frame at speed 1
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let at = vec2i(v.pos.xy);
    let p = textureLoad(in0, at, 0);
    let vel = textureLoad(in1, at, 0).xyz;
    var pos = p.xyz;
    let step = params.life_damping * params.speed;
    let h = pn_hash(vec3u(vec2u(at), u32(pn_time.frame)));
    let dither = f32(h.x >> 8u) / 16777216.0 - 0.5;
    var life = p.w - step;
    life += dither * exp2(floor(log2(max(abs(life), 1e-4))) - 10.0);
    if life <= params.life_damping {
        life = 1.0;
        pos = textureLoad(in2, at, 0).xyz;
    }
    pos += vel * params.speed;
    return vec4f(pos, life);
}
