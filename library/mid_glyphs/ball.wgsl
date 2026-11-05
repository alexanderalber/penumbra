// The glowing ball on the head of a ring of mid_glyphs: a hot core that falls off to copper and fades
// out before the picture's edge, so no edge of the quad shows. The picture is light with almost no
// alpha: it adds to what lies behind it and stays out of the render's data pass. Render it 64 x 64.

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let at = 2.0 * v.pos.xy / pn.resolution - 1.0;
    let r = length(at);
    let edge = 1.0 - smoothstep(0.55, 1.0, r);
    let core = exp(-r * r / 0.05);
    let body = exp(-r * r / 0.16);
    let halo = exp(-r * r / 0.5);
    let light = vec3f(1.0, 0.72, 0.42) * 0.8 * core + vec3f(1.0, 0.42, 0.12) * 0.55 * body + vec3f(0.55, 0.16, 0.035) * 0.28 * halo;
    return vec4f(light * edge, select(0.0, 0.003, r < 1.0));
}
