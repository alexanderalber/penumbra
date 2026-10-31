# silhouette-time-machine

A person moves in front of a projected wall. Wherever they have been, moving stripes appear,
bent and torn along the person's old outlines. Where they have never been, the wall stays
dark. The person stays dark too, so the projector does not blind them.

The trick is a `tex.time_machine` fed with a plain moving pattern. Its map is how long ago
the person was last at each pixel. So each pixel shows the pattern from the moment the person
left it.

- `/person` (`person.wgsl`) stands in for a depth camera's mask: white where the person is,
  black elsewhere. It is blocky like a Kinect's (`block`), frayed at the edges (`ragged`) and
  has stray pixels near the floor (`speckle`).
- `/seen` (`seen.wgsl`) and `/loop` remember when the person was last at each pixel. Where the
  mask is white, `/seen` writes the time; elsewhere it keeps the old value. So the latest visit
  wins.
- `/age` (`age.wgsl`) turns that into the time machine's map: black where the person is now,
  white `span` seconds after they left, and wherever they have never been.
- `/pattern` (`pattern.wgsl`) is the moving picture: stripes that scroll sideways. It knows
  nothing about the person. It holds no colours yet, only where each pixel lies in the round
  of stripes (`round` of them until the colours repeat).
- `/tm` shows each pixel of `/pattern` from as long ago as `/age` says. Next to where the person
  just walked, the delay changes quickly from pixel to pixel, so the stripes there bend and
  crowd. Where the person left at two different moments, the stripes break.
- `/out` (`show.wgsl`) adds stripes around the person and picks the colours. Each pixel looks
  along its row for the nearest pixel of the person. That distance adds `local` stripes, which
  copy the person's outline and crowd near them (`bunch`). As the pattern moves, these stripes
  run towards the person. `/out` keeps the picture only where the person has been, and black
  on the person.

Look at `/out`; `/age` shows the map.

`/tm` keeps 20 seconds of `/pattern` at 60 frames a second: `length` 1200 frames, every fourth
one kept (`step` 4), at half resolution (`scale` 0.5), about 550 MB of GPU memory. It needs
16 bits: in 8 bits the place in the round is too coarse, and the stripe edges turn ragged.
`/age` `span` has to match that length in seconds. Spots the person left longer ago all show
the pattern from 20 seconds back, so there the stripes move evenly again.

Try:

- `/out` `local` 0: the time machine's stripes alone, without the stripes around the person.
- `/pattern` `shape` `rings` or `straight`.
- `/pattern` `speed` higher: the stripes bend more strongly near the person.
- `/person` `pace` 0: the person stands still. The areas they walked through keep moving.
- Press R on `/loop` to forget where the person has been.

## With a real depth camera

penumbra has no depth camera input yet. A Kinect or a similar camera can still deliver its
mask: a small program that comes with the camera's SDK, or one of the free senders, sends it
through Spout or NDI.

1. Make the camera's program send a mask, white for everything nearer than the wall, black for
   the wall and beyond. Most senders offer that as a near and far threshold on the depth.
2. Add a `tex.spout_in` or `tex.ndi_in` and wire it into `/seen` `in0` and `/out` `in2` in
   place of `/person`.
3. Put the mask in line with the projector. The camera sees the wall from another place than
   the projector. A `tex.mapper` between the camera's picture and those two moves its corners
   until the black figure covers the person. The person stands close to the wall, so the
   wall's plane is a good enough fit.
4. A mask with holes or noise: `tex.blur` and `tex.threshold` behind the camera fill small
   holes and drop single pixels. `threshold` on `/seen` and `/out` sets what counts as the
   person.

The camera and the projector each add a few frames of delay. With fast movement the black
figure lags behind the person by that much.
