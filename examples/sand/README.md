# sand

Falling sand: grains rain down, pile up on the letters of a word and pour off their edges. The
simulation is the library component [sand](../../library/README.md#sand). Everything outside it
stands in for what an installation feeds in.

Look at `/out`. Select `/sand` to change what happens.

- **`scene`** on `/sand` picks a setup: rain on the word, an hourglass, bowls, a board of pins
  and more. Choosing a scene writes its values into the params below it, and the sand starts
  over. Then tune any of them. "back" beside the scene brings the scene's values back.
- **Key 0** starts the sand over. Click into the editor once so it gets the keys. `/keys` passes
  the key to `/sand`'s `reset`.
- `/letters` is the wall input: white where sand cannot go. In an installation this is a scan
  mask of the object.
- `/blobs` is the source input: two dots moved by `/x`, `/y`, `/x2` and `/y2`. Sand pours from
  them in scene `blob`. In an installation this is `tex.blob_track` of the people.
- `/hole` is the drain input: sand vanishes there in scene `drain`.
- `/sand`'s channel output `grains` is the number of grains.

The component's files are in `library/sand/`, copied from penumbra's library as adding it from
the add dialog does. Edit those copies to change this project only. Every param, scene and input
is described in [Simulations](../../docs/simulations.md#sand).
