# fluid

`tex.fluid` is a 2D fluid: motion pushes it, dye flows with it. Here a shader (`movers.wgsl`)
draws coloured spots wandering over black, a stand-in for a camera watching people move, so it
works without a camera.

- `/flow` (tex.optical_flow) measures how the spots move; its output goes into the fluid's
  `force` input as it is.
- `/track` (tex.blob_track) finds the spots; its `blobs` table goes into `splats`, a push and
  the spot's colour at each blob.
- `/movers` itself is the `dye`: where the picture moves, the fluid takes on its colours.
- `/view` shows the fluid over black.

Try `vorticity` (curlier), `velocity_fade` and `dye_fade` (how long it swirls and glows), and
`edges: open` to let it flow out of the picture. The `velocity` output moves other pictures
along the fluid with tex.displace (midpoint 0, weight about -0.3).

With a camera: add a `tex.video_in`, wire it into `/flow`, `/track` (or drop `/track`) and the
fluid's `dye` instead of `/movers`.
