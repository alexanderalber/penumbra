# growth

Slow growth: frost crystals start on the letters of a word, cover the picture, melt where a blob
passes and grow back. The simulation is the library component
[growth](../../library/README.md#growth). Everything outside it stands in for what an
installation feeds in.

Look at `/out`. Select `/growth` to change what happens.

- **`preset`** on `/growth` picks what grows: window frost with warm spells, one snowflake, fern
  frost, coral around the word, a lightning tree, lichen, mould, rust and more. Choosing a preset
  writes its values into the params below it, and the growth starts over. Then tune any of them.
  "back" beside the preset brings the preset's values back.
- **Key 0** starts it over. Click into the editor once so it gets the keys. `/keys` passes the key
  to `/growth`'s `reset`. Presets with `auto_restart` on also start over by themselves when the
  picture is full.
- `/word` is the mask input: growth starts on its edges. In an installation this is a scan mask
  of the object.
- `/blob` is the heat input: a soft dot moved by `/x` and `/y`. Frost melts there. In an
  installation this is `tex.blob_track` of the people.
- `/points` is the dots input: seven bright dots, drawn by `dots.wgsl`. Preset `dots` starts a
  crystal at each.
- `/growth`'s channel output `coverage` is the share of the area that is covered.

The component's files are in `library/growth/`, copied from penumbra's library as adding it from
the add dialog does. Edit those copies to change this project only. Every param, preset and input
is described in [Simulations](../../docs/simulations.md#growth).
