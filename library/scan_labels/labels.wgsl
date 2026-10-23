// A scan's faces and objects in one picture, their ragged borders smoothed: each pixel takes the
// label most common around it. in0: the faces (scan.scene's planes_camera.png or scan.regions'
// regions_camera.png through tex.image, 8-bit labels), in1: the objects (objects_camera.png).
// Out: red the face (k / 255), green the object (k / 255), as plain numbers, so the shaders after
// it read them with round(value * 255).
// @param radius int = 2 0..4 -- how far the vote reaches, pixels
// @param keep int = 0 0..255 -- the one object kept, 0 for all; the others are left out,
// neither face nor object
// @param fill int = 0 0..255 -- a rectified face (in2: its uv_K_camera.exr) whose outline is
// filled where the scan left a hole (a glossy top decodes in patches only): 0 for none

fn label(t: texture_2d<f32>, p: vec2i) -> i32 {
    let size = vec2i(textureDimensions(t));
    let c = textureLoad(t, clamp(p, vec2i(0), size - 1), 0).r;
    let s = select(1.055 * pow(max(c, 0.0), 1.0 / 2.4) - 0.055, c * 12.92, c <= 0.0031308);
    return i32(round(s * 255.0));
}

// Faces by number, objects 256 and up.
fn id(p: vec2i) -> i32 {
    let o = label(in1, p);
    return select(label(in0, p), 256 + o, o > 0);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = vec2i(v.pos.xy);
    let r = clamp(params.radius, 0, 4);
    var ids: array<i32, 81>;
    var n = 0;
    for (var dy = -r; dy <= r; dy++) {
        for (var dx = -r; dx <= r; dx++) {
            ids[n] = id(p + vec2i(dx, dy));
            n++;
        }
    }
    var best = id(p);
    var most = 0;
    for (var i = 0; i < n; i++) {
        var count = 0;
        for (var j = 0; j < n; j++) {
            count += select(0, 1, ids[j] == ids[i]);
        }
        if (count > most) {
            most = count;
            best = ids[i];
        }
    }
    var face = select(best, 0, best >= 256);
    var object = select(0, best - 256, best >= 256);
    if (params.keep > 0 && object != params.keep) {
        object = 0;
    }
    // Inside the rectified face's outline, what no face claims and is not the kept object is
    // that face (its uv's blue is 1 on it, also where nothing was measured, 0.5 on an object).
    if (params.fill > 0 && face == 0 && object == 0 && textureLoad(in2, p, 0).b > 0.4) {
        face = params.fill;
    }
    return vec4f(f32(face) / 255.0, f32(object) / 255.0, 0.0, 1.0);
}
