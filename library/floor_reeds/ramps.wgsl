// The plants' two line colours side by side: the left half of the picture is in0 (the jade ramp),
// the right half in1 (the copper ramp). A model's u picks the half. Render 2 x 64.

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let y = i32(v.pos.y);
    if v.pos.x < 1.0 {
        return textureLoad(in0, vec2i(0, y), 0);
    }
    return textureLoad(in1, vec2i(0, y), 0);
}
