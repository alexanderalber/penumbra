// Passes in1 on, and only once in0 exists. The element's models wear a picture that a shader draws,
// and that shader compiles in the background the first time the element shows. A material without
// its picture draws plain white, so the copies wait here until the picture is there. Set the node's
// resolution to `input`: a tex.shader that takes its size from in0 waits for in0. in0 is the picture
// scaled to the size of in1.

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    return textureLoad(in1, vec2i(v.pos.xy), 0);
}
