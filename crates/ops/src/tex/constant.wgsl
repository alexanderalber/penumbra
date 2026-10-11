// tex.constant: one colour everywhere.

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let c = params.color;
    return vec4f(c.rgb * c.a, c.a);
}
