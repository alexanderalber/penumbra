// One step of the trail: add what the agents deposited, spread it to the neighbours, let it fade.
// rgb: the trail, coloured by where the agents head (r + g + b is the amount). a: the smell of
// food, which reaches farther and is not drawn. The agents smell r + g + b + a.
//
// in0: the trail of the last frame (the tex.feedback). in1: the deposit (tex.scatter: per cell
// the agents there, rgb weighted by heading, alpha their number). in2: fields.wgsl (g walls,
// b above 0 food, below 0 an extra fade).
//
// One agent leaves `deposit` times (cells / agents) / 2, so the trail is as strong at any
// agent count and size as with 262 144 agents on 960 x 540 cells.
//
// @param agents int = 256 1..4096 -- follows the component's agents (units of 1024)
// @param pause bool = false -- holds the trail as it is
// @param deposit float = 1 0..20 -- trail one agent leaves per step
// @param decay float = 0.1 0..1 -- share of the trail that fades per step
// @param diffusion float = 1 0..1 -- 1: each step every cell takes the mean of its 3 x 3 neighbourhood
// @param food_decay float = 0.01 0.001..1 -- share of the food smell that fades per step; lower reaches farther

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = vec2i(v.pos.xy);
    if params.pause != 0u {
        return textureLoad(in0, p, 0);
    }
    let size = vec2i(textureDimensions(in0));
    let cells = f32(size.x * size.y);
    let dep = params.deposit * 0.5 * cells / (1024.0 * f32(max(params.agents, 1)));
    var mean = vec4f(0.0);
    var own = vec4f(0.0);
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let q = (p + vec2i(x, y) + size) % size;
            let f = textureLoad(in2, q, 0);
            // Food is added as plain trail (a third to each heading colour) and as far smell.
            let food = max(f.b, 0.0);
            let t = textureLoad(in0, q, 0) + vec4f(dep * textureLoad(in1, q, 0).rgb + vec3f(food / 3.0), food);
            mean += t;
            if x == 0 && y == 0 {
                own = t;
            }
        }
    }
    mean = mean / 9.0;
    var t = vec4f((1.0 - params.decay) * mix(own.rgb, mean.rgb, params.diffusion), (1.0 - params.food_decay) * mean.a);
    let f = textureLoad(in2, p, 0);
    t = vec4f(t.rgb * (1.0 - clamp(-f.b, 0.0, 1.0)), t.a);
    if f.g > 0.5 {
        t = vec4f(0.0);
    }
    return max(t, vec4f(0.0));
}
