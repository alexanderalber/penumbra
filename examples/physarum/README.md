# physarum

Physarum, the slime mould: many agents follow a trail and lay one, and the trail grows into a
network of glowing veins. A moving blob draws a vein after it or pushes the network away, and
the network links a few food dots. The simulation is the library component
[physarum](../../library/README.md#physarum). Everything outside it stands in for what an
installation feeds in.

Look at `/out`. Select `/physarum` to change what happens.

- **`preset`** on `/physarum` picks the organism: fine lace, smooth veins, soap foam, dots,
  spirals, fire, worms, foam that writes the word, lace that flows around the word, veins that
  link the food dots, and veins coloured by direction. Choosing a preset writes its values into
  the params below it. Then tune any of them. "back" beside the preset brings the preset's
  values back.
- A new preset **carries on** from the network that is there: the agents change their ways,
  and the network reshapes within a second or two. With **`fresh`** on, a new preset starts
  over instead.
- **Key 0** starts it over. Click into the editor once so it gets the keys. `/keys` passes the
  key to `/physarum`'s `reset`.
- `/word` is the mask input. `mask_mode` says what it does; presets `word` and `walls` use it.
  In an installation this is a scan mask of the object.
- `/blob` is the people input, moved by `/x` and `/y`. `people_mode` says what it does:
  attract a vein, repel the network, act as a wall, or feed the trail. In an installation this
  is `tex.blob_track` of the people.
- `/points` is the food input: twelve dots, drawn by `points.wgsl`. Preset `network` links them
  with veins. In an installation this is a picture of things to link.
- The channel outputs `coverage` and `alive` say how much of the area has visible trail and how
  many agents live.

The component's files are in `library/physarum/`, copied from penumbra's library as adding it
from the add dialog does. Edit those copies to change this project only. Every param, preset and
input is described in [Simulations](../../docs/simulations.md#physarum).
