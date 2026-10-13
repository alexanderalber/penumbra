# curl-noise

65 536 particles drift through a curl-noise flow, each drawn as a stick pointing along its
velocity, growing and shrinking over its life, the whole swarm pulsing in length.

The simulation runs on the GPU, one particle per texel of a 256 x 256 texture:

- `/ini` (`ini.wgsl`) holds where particles start: a position in rgb, a random life in alpha.
- `/pos` (`pos.wgsl`) moves each particle by its velocity and ages it; a spent one starts again
  at its place in `/ini`. `/pos_last` is a `tex.feedback` that hands the positions to the next
  frame.
- `/vel` (`vel.wgsl`) pushes each particle along the curl of a noise field, which swirls without
  bunching up, and damps the velocity. `/vel_last` loops it back like `/pos_last`.

`/sticks` is a `geo.texinstance`: one copy of `/stick` per texel, placed by `/pos`, turned
towards `/vel`, stretched by `/scale` (the size times `/pulse`) and coloured by `/palette`.
`/render` draws them lit by `/light`, `/out` puts them over a dark radial `/background`.

The swarm needs half a minute or so to spread out from its start.

Try:

- `/palette` `keys`: other colours (the sticks take them by column).
- `/vel` `noise`: the first value is the push's strength, the second how small the swirls are.
- `/pulse` `rate` and `amplitude`, or bind `/scale` `pulse` to a sound level instead.
- `/box` `size` for thicker or longer sticks.
