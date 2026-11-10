# lenia

Lenia, a continuous cellular automaton: soft creatures glide, swarm, split and die on a dark
ground. A moving blob drops new gliders where it passes. The word LENIA is the mask: presets
`home` and `word` use it. The simulation is the library component
[lenia](../../library/README.md#lenia). Everything outside it stands in for what an
installation feeds in.

Look at `/out`. Select `/lenia` to change what happens.

- **`preset`** on `/lenia` picks the creatures: gliders, a jiggling swarm, spiky tissue, a
  labyrinth of worms, SmoothLife rings, three kinds that chase each other, the three kinds inside
  the word only, worms that write the word by their size, and gliders that appear only behind
  the blob. Choosing a preset writes its values into the params below it, and the world starts
  over. Then tune any of them. "back" beside the preset brings the preset's values back.
- **`morph`** on: a new preset acts on the world that is there instead of starting over. Most
  creatures die or turn into the new preset's texture within seconds.
- **Key 0** starts it over. Click into the editor once so it gets the keys. `/keys` passes the key
  to `/lenia`'s `reset`.
- `/word` is the mask input. `mask_mode` says what it does. In an installation this is a scan
  mask of the object.
- `/blob` is the people input, moved by `/x` and `/y`. `people_mode` says what it does: drop new
  creatures, feed life, kill it, or push it away. In an installation this is `tex.blob_track` of
  the people.
- The channel outputs `coverage` and `activity` say how much of the area is alive and how much
  the world still changes.

The component's files are in `library/lenia/`, copied from penumbra's library as adding it from
the add dialog does. Edit those copies to change this project only. Every param, preset and
input is described in [Simulations](../../docs/simulations.md#lenia).
