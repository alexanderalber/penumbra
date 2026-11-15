# liquid

`tex.liquid` is a liquid seen from the side: it pours, lands on obstacles, pools and runs off.
Here three taps pour water onto the word "penumbra". The water runs over the letters and drips
into a basin with a drain in its floor. A gusty wind blows the streams sideways, and a paddle
sweeps through the basin. At the right, sand pours onto a shelf, heaps up and slides off.

Look at `/out`. The basin fills within about 5 seconds and then keeps its level: the drain lets
out what the taps pour in.

- `/tap1` to `/tap3` and `/sand_tap` are `tex.liquid_emitter` nodes, wired into `emitter0` to
  `emitter3` of `/liquid`. `material` on `/sand_tap` is `sand`.
- `/word` (a `tex.text`) and `/shapes` (a `tex.shapes`: the basin in two halves with the drain
  between them, and the shelf) are put together in `/solid`. `/solid` goes into `obstacles`.
- `edges` on `/liquid` is `open`. Water that leaves the picture, through the drain or past the
  basin, pours again from the taps. The particles never run out: after 3 minutes the picture
  looks as it does after 20 seconds.
- `/wind` goes into `force`. It is a `tex.channel_mix` of `/turbulence`, a morphing
  `tex.noise`, with green set to 0, so the wind blows sideways only. `/gust`, a slow
  `chan.lfo`, is bound to the red `bias` of `/wind`: every 12.5 seconds the wind blows to the
  right and then to the left.
- `/paddle` goes into `splats`. It is a `table.from_chan` of three channels: `x` from `/sweep`,
  `vx` from `/sweep_speed` (the speed of `/sweep`, in widths per second) and `y` from `/depth`.
  `splat_motion` on `/liquid` is `set`, so the water within the paddle's radius moves with it.
- `/wet` lays the `out` of `/liquid` over `/scene`, the sky and the solids in grey. `out` is
  transparent around the liquid, so no key or mask is needed.
- `/glow` blurs the `foam` output, and `/out` adds it on top.
- `friction` on `/liquid` is 1, so the water clings to the letters a little.

Try on `/liquid`: `force` (0 stops the wind, more blows harder), `splat_motion` `push` (the
paddle only nudges the water), `viscosity` (0.001 pours like honey), `reset` (empties the
picture). Try `material` `snow` on `/sand_tap`, and `amplitude` on `/gust`. A
`tex.optical_flow` of a camera goes into `force` as it is, and a `tex.blob_track` table into
`splats`, as in the [fluid](../fluid/README.md) example.

It costs about 3 ms per frame on an RTX 3090 (power limit 80 %), measured headless at 60 fps.

Not in this example: water that wets sand (sand in water hangs in it as specks), a second
liquid of another colour, and bodies from `physics.rigid_bodies` that float. The page of
[tex.liquid](../../docs/nodes/tex.liquid.md) has the units, the limits and the costs.
