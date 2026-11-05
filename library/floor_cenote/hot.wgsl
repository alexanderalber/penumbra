// One hot spot in floor_cenote's pool: a pale core that falls off to jade and fades out before the
// picture's edge. It is light with almost no alpha: it adds to the pool and stays out of the
// render's data pass. Render it 128 x 128. Its brightness comes from the placement shader.

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let at = 2.0 * v.pos.xy / pn.resolution - 1.0;
    let r = length(at);
    let edge = 1.0 - smoothstep(0.6, 1.0, r);
    let core = exp(-r * r / 0.035);
    let body = exp(-r * r / 0.13);
    let halo = exp(-r * r / 0.45);
    let light = vec3f(0.8, 1.0, 0.88) * 0.9 * core + vec3f(0.22, 1.0, 0.68) * 0.6 * body + vec3f(0.03, 0.5, 0.33) * 0.3 * halo;
    return vec4f(light * edge, select(0.0, 0.003, r < 1.0));
}
