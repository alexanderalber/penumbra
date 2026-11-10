// The grains' start: none of them on the plate yet (blue -1). `grains` sprinkles each one onto a
// random place of the plate in its first step. The size of this node is the number of grains:
// 1024 across, the component's `grains` down.
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    return vec4f(0.0, 0.0, -1.0, 0.0);
}
