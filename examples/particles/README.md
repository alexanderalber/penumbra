# particles

Sparks rise from the word "penumbra": particles born on the letters, stirred by a curl noise,
blown up by a wind, glowing yellow at birth and dark red as they die. Each spark leaves a short
trail, and a few of them join their neighbours with thin lines.

Look at `/out`.

It shows the usual particle recipe of TouchDesigner's POPs, built from `pts` nodes:

- `/text` is the word as a solid. `/emitter` (`pts.from_geo`, mode `surface`) scatters 20 000
  points over its surface, each with the surface's normal there.
- `/sparks` (`pts.particles`) gives birth to 8000 particles a second on these points. Each lives
  about 2 s.
- `/curl` (`pts.noise`, flavour `curl`, on `force`) stirs them like smoke. `/wind` (`pts.force`,
  kind `wind`) pulls them up and a little to the right.
- `/end` is the end of the loop. It is wired back into `previous` of `/sparks`: each step starts
  from what `/end` held a step ago. TouchDesigner calls it the target.
- `/fade` (`pts.math`) colours each spark by `age / life`: pale yellow at birth, dark red and
  dim as it dies.

Three ways to draw them, added together in `/render`:

- `/dots`: a soft sprite at every spark (`softness` 1, `blend` add), so dense places glow.
- `/trail` keeps the last 12 places of each spark. `/taper` thins each trail to its tail.
  `/streaks` draws the trails as ribbons.
- `/few` keeps 5 % of the sparks. `/plexus` (`pts.proximity`) joins each of them to up to three
  neighbours within 0.3 m. `/strands` darkens the long lines, `/web` draws them.

`/out` puts the render over a dark radial `/background`. Added light covers nothing, so the
render alone stays see-through; the composite gives it its background.

About 16 000 sparks live at once, and their trails hold about 190 000 points. On an RTX 3090
at a 280 W power limit the network takes about 5.6 ms a frame at 1920 x 1080 (stepped, all GPU
work included). Most of it is the CPU cooking 25 nodes; the GPU is busy about a quarter of the
time. The show laptop (RTX 3060 mobile) has not run it yet.

Try:

- `/text` `text`: another word. The sparks follow its letters.
- `/wind` `strength`: 0 leaves the sparks floating in the curl, 2 blows them up and away.
- `/trail` `length`: 30 gives long streaks.

The page [Points](../../docs/points.md) explains each step and the other recipes: trails,
plexus, flocks, pictures as points.
