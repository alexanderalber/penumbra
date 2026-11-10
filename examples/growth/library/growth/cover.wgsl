// Which texels are covered, for the measure `coverage` and the restart when it is full.
// r: 1 where something grows here (ice, a branch, a living colony) and may grow, else 0.
// g: 1 where growth may happen at all (everywhere, inside or outside the mask, as `area` says).
// The share covered is the mean of r over the mean of g.
//
// in0: the state. in1: the object mask, any size.
//
// @param model choice(frost|branch|lichen) = frost -- follows the component's model
// @param area choice(all|inside|outside) = all -- follows the component's area

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let n = vec2i(textureDimensions(in0));
    let s = textureLoad(in0, clamp(vec2i(v.pos.xy), vec2i(0), n - vec2i(1)), 0);
    let lum = clamp(pn_luma(textureSampleLevel(in1, pn_clamp, v.uv, 0.0).rgb), 0.0, 1.0);
    var ok = 1.0;
    if params.area == area_inside { ok = select(0.0, 1.0, lum > 0.5); }
    if params.area == area_outside { ok = select(0.0, 1.0, lum <= 0.5); }
    var grown = select(0.0, 1.0, s.r > 0.5);
    if params.model == model_lichen { grown = select(0.0, 1.0, s.r > 0.0); }
    return vec4f(grown * ok, ok, 0.0, 1.0);
}
