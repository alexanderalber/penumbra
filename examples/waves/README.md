# waves

Ripples on water: rain falls on a tiled pool, the rings bounce off the letters of a word, and a
moving blob pushes the water. The simulation is the library component
[waves](../../library/README.md#waves). Everything outside it stands in for what an installation
feeds in.

Look at `/out`. Select `/waves` to change what happens.

- **`scene`** on `/waves` picks a setup: rain on a pool, one drop, a double slit, two bobbing
  points, a straight wave past the word, a wake, a drop on each beat, rain among posts. Choosing
  a scene writes its values into the params below it, and the water starts over. Then tune any
  of them. "back" beside the scene brings the scene's values back.
- **Key 0** starts the water over. Click into the editor once so it gets the keys. `/keys`
  passes the key to `/waves`'s `reset`.
- `/word` is the mask input: the word stands in the water, and waves reflect off it and bend
  around it. In an installation this is a scan mask of the object.
- `/blob` is the people input, moved by `/x` and `/y`. It pushes the water where it moves. In an
  installation this is `tex.blob_track` of the people.
- `/metronome` is the beat input, 100 beats a minute. Scene `beat` drops a drop on each beat. In
  an installation this is a beat from the music.
- `/waves`'s channel output `ripple` says how much the water moves: 0 when still, about 0.04
  in scene `pond`.

The component's files are in `library/waves/`, copied from penumbra's library as adding it from
the add dialog does. Edit those copies to change this project only. Every param, scene and input
is described in [Simulations](../../docs/simulations.md#waves).
