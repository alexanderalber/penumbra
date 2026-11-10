// What the inputs mean to the agents, at the simulation size (in0 is the world):
//   r: added to what the agents smell (negative repels), g: 1 where agents may not go,
//   b: above 0 trail added every step (food, fed by people); below 0 the share of the trail that
//      fades there each step on top of the decay (repelling people),
//   a: where new agents are born when born is food (the food input and a mask used as food).
// in1: the object mask, in2: people, in3: food. Each may have any size; an unwired one is black.
//
// @param mask_mode choice(off|attract|outline|wall|inside|food) = off -- what the mask input does; attract: the agents follow its shape; outline: they follow its edge; wall: they flow around it; inside: they live only on it; food: it feeds the trail
// @param people_mode choice(off|repel|attract|wall|feed) = attract -- what the people input does; repel: the agents turn away; attract: a vein reaches for them; wall: the agents flow around them; feed: they feed the trail
// @param strength float = 1 0..4 -- how strongly the people act
// @param pull float = 300 0..1000 -- how strongly the mask attracts with mask_mode attract and outline
// @param food_rate float = 0 0..20 -- trail the food input adds per step where it is bright
// @param line float = 8 0.5..40 -- mask_mode outline: width of the edge, in pixels of the mask
// @param level float = 0.5 0.05..0.95 -- brightness above which an input counts as lit

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let uv = v.uv;
    let on_mask = params.mask_mode;
    let on_people = params.people_mode;
    let food_rate = params.food_rate;
    let m = pn_luma(textureSampleLevel(in1, pn_clamp, uv, 0.0).rgb);
    let person = clamp(pn_luma(textureSampleLevel(in2, pn_clamp, uv, 0.0).rgb), 0.0, 1.0);
    let food_in = clamp(pn_luma(textureSampleLevel(in3, pn_clamp, uv, 0.0).rgb), 0.0, 1.0);

    var smell = 0.0;
    var wall = 0.0;
    var food = food_rate * food_in;
    var fade = 0.0;
    var born = food_in;
    if on_mask == mask_mode_attract {
        smell += params.pull * m;
    } else if on_mask == mask_mode_outline {
        let d = params.line / vec2f(textureDimensions(in1));
        var lo = m;
        var hi = m;
        for (var i = 0; i < 8; i++) {
            let w = f32(i) / 8.0 * 6.2831853;
            let s = pn_luma(textureSampleLevel(in1, pn_clamp, uv + d * vec2f(cos(w), sin(w)), 0.0).rgb);
            lo = min(lo, s);
            hi = max(hi, s);
        }
        smell += params.pull * (hi - lo);
    } else if on_mask == mask_mode_wall {
        wall = select(0.0, 1.0, m > params.level);
    } else if on_mask == mask_mode_inside {
        wall = select(1.0, 0.0, m > params.level);
    } else if on_mask == mask_mode_food {
        food += max(food_rate, 1.0) * m;
        born += m;
    }
    let ps = params.strength * person;
    if on_people == people_mode_repel {
        smell -= 400.0 * ps;
        fade = 0.25 * min(ps, 1.0);
    } else if on_people == people_mode_attract {
        smell += 300.0 * ps;
        food += 5.0 * ps;
    } else if on_people == people_mode_wall {
        wall = max(wall, select(0.0, 1.0, person > params.level));
    } else if on_people == people_mode_feed {
        food += 5.0 * ps;
    }
    return vec4f(smell, wall, select(food, -fade, fade > 0.0), born);
}
