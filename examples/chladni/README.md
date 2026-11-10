# chladni

Chladni figures: sand on a vibrating plate jumps away from the parts that move and gathers on
the lines that stand still. A melody changes the figure every 2 s. The simulation is the library
component [chladni](../../library/README.md#chladni). Everything outside it stands in for what an
installation feeds in.

Look at `/out`. Select `/chladni` to change what happens.

- **`preset`** on `/chladni` picks the plate and its look: a square plate, a round one, a plate
  cut to the guitar outline, gold, ice or ember sand, or only the nodal lines. Choosing a preset
  writes its values into the params below it, and the sand starts over. Then tune any of them.
  "back" beside the preset brings the preset's values back.
- **Key 0** starts the sand over. Click into the editor once so it gets the keys. `/keys` passes
  the key to `/chladni`'s `reset`.
- **The tone.** `/walk_lfo` and `/walk` step through the notes 50 to 61, one every 2 s.
  `/semitone`, `/ratio` and `/hz` turn the note into a frequency. `/tone` plays it as a sine
  tone, which no speaker plays. `/pitch` (`chan.audio_pitch`) hears it and sends the channel
  `note` into `/chladni`'s input `tone`. Each semitone from `lowest` (48, C3) up is the next
  figure.
- `/shape` is the shape input: a guitar outline, drawn by `outline.wgsl`. Its `shape` param also
  has a star, a square, a circle, and `picture`, the letter of `/letter`. The presets `shape`,
  `ember` and `motion` use it. In an installation this is a scan mask of the object.
- `/chladni`'s channel output `motion` is how far the grains move per step.

**With a microphone.** Add an `audio.device_in` node and wire it into `/pitch`'s input `audio`,
in place of `/tone`. `/walk_lfo`, `/walk`, `/semitone`, `/ratio`, `/hz` and `/tone` can then
go. Sing, whistle or play one note at a time. Chords and loud drums confuse the pitch tracker.
If your voice sits higher, raise `lowest` on `/chladni`. The microphone opens as soon as the
node exists.

The component's files are in `library/chladni/`, copied from penumbra's library as adding it
from the add dialog does. Edit those copies to change this project only. Every param, preset
and input is described in [Simulations](../../docs/simulations.md#chladni).
