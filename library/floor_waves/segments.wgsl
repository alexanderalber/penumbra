// The segments of the wave lines: segment i of line j joins points i and i + 1 of the height
// field (field.wgsl in in0; render this one a column narrower). A segment whose ends both stand
// under 3 cm is left out, so the flat lane and flattened lines draw nothing, and a low segment
// is dimmer: it brightens up to `BRIGHT` high, so a line that sinks towards the floor fades out
// instead of breaking into dashes.
// `what` place: the segment's middle. aim: its direction. scale: its length along y (the
// segment model is one unit long along y). light: its brightness.
// @param what choice(place|aim|scale|light) = place -- what this node writes

// Heights under this, in metres, are left out.
const LOW: f32 = 0.03;
// From this height up, in metres, a segment has its full brightness.
const BRIGHT: f32 = 0.2;

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let cell = vec2i(v.pos.xy);
    let a = textureLoad(in0, cell, 0);
    let b = textureLoad(in0, cell + vec2i(1, 0), 0);
    let shown = max(a.a, b.a) >= LOW;
    if params.what == what_place {
        return vec4f(0.5 * (a.xyz + b.xyz), 1.0);
    }
    let d = b.xyz - a.xyz;
    if params.what == what_aim {
        return vec4f(select(vec3f(1.0, 0.0, 0.0), d, length(d) > 1e-4), 1.0);
    }
    if params.what == what_light {
        let l = smoothstep(LOW, BRIGHT, max(a.a, b.a));
        return vec4f(l, l, l, 1.0);
    }
    let s = select(0.0, 1.0, shown);
    return vec4f(s, s * length(d), s, 1.0);
}
