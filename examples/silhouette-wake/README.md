# silhouette-wake

A person moves in front of a projected wall. Stripes run across the whole wall without ever
stopping, and the person's movement bends and tears them into copies of their old outlines. The
person stays dark, so the projector does not blind them.

Every pixel counts stripes at the same steady rate. A visit from the person sets its count back.
So each stripe keeps moving, and the count on the wall is the time since the person left.

- `/person` (`person.wgsl`) stands in for a depth camera's mask: white where the person is, black
  elsewhere. It is blocky like a Kinect's (`block`), frayed at the edges (`ragged`) and has stray
  pixels near the floor (`speckle`). It runs quickly to the right, then goes back slowly to the
  left with small steps.
- `/seen` (`seen.wgsl`) and `/loop` remember when the person was last at each pixel. Where the
  mask is white, `/seen` writes the time; elsewhere it keeps the old value. So the latest visit
  wins.
- `/out` (`show.wgsl`) makes the stripes from that time. A line of equal count is an outline the
  person had a moment ago, and these lines follow the person's path. They lie far apart where the
  person moved fast and close together where they moved slowly. Where the person came back to a
  place at a different moment, the stripes tear. `across` adds plain upright stripes running to
  the right. They fill the parts the person has not reached yet.

Look at `/out`.

The picture is square, as the stripes and the person fill it best that way. Change `width` and
`height` on `/person` and `/black` together for another shape.

Try:

- `/out` `across` 0: only the stripes the person leaves.
- `/out` `speed`: every stripe at once, also the ones already on the wall.
- `/out` `gap` 0: colour bands without the black.
- `/person` `pace` 0: the person stands still and the stripes run on.
- Press R on `/loop` to forget where the person has been.

## With a real depth camera

penumbra has no depth camera input yet. A Kinect or a similar camera can still deliver its mask: a
small program that comes with the camera's SDK, or one of the free senders, sends it through Spout
or NDI.

1. Make the camera's program send a mask, white for everything nearer than the wall, black for the
   wall and beyond. Most senders offer that as a near and far threshold on the depth.
2. Add a `tex.spout_in` or `tex.ndi_in` and wire it into `/seen` `in0` and `/out` `in1` in place
   of `/person`.
3. Put the mask in line with the projector. The camera sees the wall from another place than the
   projector. A `tex.mapper` between the camera's picture and those two moves its corners until
   the black figure covers the person. The person stands close to the wall, so the wall's plane is
   a good enough fit.
4. A mask with holes or noise: `tex.blur` and `tex.threshold` behind the camera fill small holes
   and drop single pixels. `threshold` on `/seen` and `/out` sets what counts as the person.

The camera and the projector each add a few frames of delay. With fast movement the black figure
lags behind the person by that much.
