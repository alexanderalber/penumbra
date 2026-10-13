# feedback

`tex.feedback` closes a loop: its `previous` input sees the frame the end of the chain made a
frame earlier (the dashed wire from `/mix`). Each pass zooms and turns the last frame a little
(`/drift`), dims it (`/fade`) and turns its hue (`/shift`); `/mix` draws the text on top and
hands the result back to the loop. So every frame leaves a trail that drifts outwards, fades
and changes colour.

`/x` and `/y` are LFOs bound to the text's position, which is why it wanders.

Try:

- `/fade` `brightness` closer to 1 for longer trails, lower for shorter ones.
- `/drift` `rotate` negative, or `scale` below 1 so the trails fall inwards.
- `/loop` `reset` on and off to clear the loop.
