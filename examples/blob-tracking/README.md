# blob-tracking

`tex.blobtrack` finds blobs and follows them, each keeping its id while it moves. Here it
tracks spots of light that a shader (`spots.wgsl`) draws, so it works without a camera:
`detect` is `bright`, so whatever is brighter than `threshold` counts.

- `/track` shows the picture with each blob's box and centre; its `blobs` output is a table of
  all blobs, its `chans` output has `count` and, per slot, `id`, `x`, `y` and `area`.
- `/slots` passes those channels on; `/first` and `/second` are bound to the positions of the
  blobs in slots 1 and 2, so the numbers ride on them.

With a camera: add a `tex.videoin` (Tab > tex > videoin), wire it into `/track` instead of
`/spots`, set `detect` to `background`, step out of the picture and press **learn background**.
See `docs/tracking.md` and `docs/camera.md`.
