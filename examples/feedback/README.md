# feedback

`tex.feedback` closes a loop: its `previous` input sees the frame the end of the chain made a
frame earlier (the dashed wire from `/mix`). Each pass zooms and turns the last frame a little
(`/drift`), softens it (`/soften`), dims it (`/fade`) and turns its hue (`/shift`); `/mix` lays a
blurred copy of the text on top (`/glow`) and hands the result back to the loop. So every frame
leaves a trail that drifts outwards, fades and changes colour.

The steps are small and the loop only sees soft edges, so the trail stays smooth. With a larger
`/drift` step or the sharp text in the loop, each pass's copy shows as a separate edge.

`/show` draws the sharp text last, over a dark halo (`/dark`, `/shadow`), so the word stays
readable on its own trail.

`/x` and `/y` are LFOs bound to the text's position. They make it wander.

Try:

- `/fade` `brightness` closer to 1 for longer trails, lower for shorter ones.
- `/soften` `size` 0 and `/mix` fed by `/seed` instead of `/glow`, to see the separate copies.
- `/drift` `rotate` positive to turn the other way, or `scale` below 1 so the trails fall inwards.
- `/loop` `reset` on and off to clear the loop.
