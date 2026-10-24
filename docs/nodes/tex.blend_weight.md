## What it is for

Where two projectors light the same surface, each shows only its share of the picture, so the
overlap is not twice as bright. `scan.blend` measures those shares as `weight_N.png`, one per
projector; this node applies one of them to the picture a projector gets, after the picture has
been moved into that projector's pixels (`tex.remap`).

## Smooth and detail

In `smooth` mode the whole picture is multiplied by the weight: a cross-fade from one projector
to the other. That is right for areas of colour and soft light. But two projectors never agree
to the pixel, so a thin line, an outline, a grid or text in the overlap shows up twice, a pixel
or two apart, each copy at half brightness.

In `detail` mode the picture is split in two. Its blurred version (radius `size`) is weighted by
the blend weight as before; what the blur took away, the lines and edges, is weighted by the
detail weight wired into `detail`: `scan.blend`'s `detail_N.png`, which is 1 where this projector
is the sharpest of those lighting the spot and 0 elsewhere. So every line comes from one
projector only and stays crisp, while brightness still crosses over smoothly.

## Choosing size

`size` is where small ends, in projector pixels. Take a little more than the projectors disagree
by (8 is a good start). Larger does no harm to the lines, but light cannot be negative: where a
thin dark gap lies right beside something bright, the other projector's blurred share fills a
faint halo into it, and that halo is as wide as `size`.

Without a `detail` input, detail mode weights like smooth mode and says so in a warning.
