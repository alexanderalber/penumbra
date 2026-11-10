// What the inputs mean to the simulation, at the simulation size (in0 is the world):
//   r: the mask (0..1), g: the people (0..1, times strength), b: how fast life dies here (the
//   mask in mask_mode home and wall), a: 1 where the rules hold as set, 0 where the outside
//   rules do (mask_mode rules), 1 everywhere otherwise.
// in1: the object mask, in2: people. Each may have any size; an unwired one is black.
//
// @param mask_mode choice(off|home|wall|rules) = off -- what the mask does; off: nothing; home: life only where it is bright; wall: nothing lives where it is bright; rules: other-sized creatures where it is dark (outside_size)
// @param strength float = 1 0..2 -- how strongly the people act

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let mode = params.mask_mode;
    let m = clamp(pn_luma(textureSampleLevel(in1, pn_clamp, v.uv, 0.0).rgb), 0.0, 1.0);
    let person = clamp(pn_luma(textureSampleLevel(in2, pn_clamp, v.uv, 0.0).rgb), 0.0, 1.0) * params.strength;
    var death = 0.0;
    var inside = 1.0;
    if mode == mask_mode_home {
        death = 1.0 - m;
    } else if mode == mask_mode_wall {
        death = m;
    } else if mode == mask_mode_rules {
        inside = m;
    }
    return vec4f(m, person, death, inside);
}
