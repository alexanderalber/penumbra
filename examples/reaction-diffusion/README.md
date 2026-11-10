# reaction-diffusion

Reaction-diffusion (Gray-Scott): two chemicals react and spread, and the picture becomes branching
coral, a labyrinth, dividing spots or worms. A moving blob wipes a path that grows back. The
simulation is the library component [reaction_diffusion](../../library/README.md#reaction_diffusion).
Everything outside it stands in for what an installation feeds in.

Look at `/out`. Select `/reaction_diffusion` to change what happens.

- **`preset`** on `/reaction_diffusion` picks the pattern: coral, mitosis, solitons, maze, worms
  and more, coral tubes only inside the word, a maze in the word with dots around it, and a map
  of every pattern side by side. Choosing a preset writes its values into the params below it,
  and the pattern starts over. Then tune any of them. "back" beside the preset brings the
  preset's values back.
- **`morph`** on: a new preset grows out of the pattern that is there. Feed and kill glide to the
  new preset's values over `glide` seconds (6). Maze to solitons and coral to mitosis work well.
- **`outside`** says what grows where the word is dark: nothing (`dead`, so the pattern lives on
  the letters only), or another pattern. Choose it after the preset: a preset writes its own.
- **Key 0** starts it over. Click into the editor once so it gets the keys. `/keys` passes the key
  to `/reaction_diffusion`'s `reset`. A pattern that has died starts over by itself after 3 s.
- `/word` is the mask input. In an installation this is a scan mask of the object.
- `/blob` is the people input, moved by `/x` and `/y`. `touch` says what it does: wipe the
  pattern, seed new pattern, or let it die away. In an installation this is `tex.blob_track` of
  the people.
- The channel outputs `coverage` and `activity` say how much of the surface has pattern and how
  much it still changes.

The component's files are in `library/reaction_diffusion/`, copied from penumbra's library as
adding it from the add dialog does. Edit those copies to change this project only. Every param,
preset and input is described in [Simulations](../../docs/simulations.md#reaction-diffusion).
