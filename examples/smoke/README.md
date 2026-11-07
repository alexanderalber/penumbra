# smoke

`vol.smoke` is smoke in a box in space. Three `vol.emitter` nodes blow smoke, heat and fuel into
the box. `tex.raymarch` draws the result, and hot smoke glows in the colours of `/colors`.

The smoke needs a moment to develop: look at `/out` after about 10 seconds.

- `/left` is a ball that blows to the right.
- `/drifting` is a ball that moves from side to side. `/sway`, a slow chan.lfo, is bound to its
  `position` x. Its `correction_rate` of 2 and low `fuel` keep the moving rim clean.
- `/wide` is a flat box. Higher up, the smoke of `/drifting` and `/wide` mixes.
- `/smoke` holds the box: 8 m by 4.5 m by 3 m at `resolution` 160, about 860 000 cells.
- `/march` shows the lower middle of the box, 1280 by 720 pixels. `/black` under it in `/out`
  makes the picture opaque.

Keep an emitter at least 1 m away from the open box edge behind it. Closer than that, air comes in
through the edge as a straight jet and leaves straight streaks in the smoke.

Try on `/smoke`: `buoyancy` (how fast it climbs), `vorticity_strength` (more curls), `resolution`
(finer smoke, about eight times the cost at double). Try on `/march`: `glow`, `thickness`, `color`,
and the keys of `/colors` for the look. More emitters, the columns of a table and a table made from
a tracker: [the recipe](../../docs/nodes/vol.smoke.md).
