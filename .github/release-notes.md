## penumbra 0.10.0

Builds for Windows x64, macOS on Apple silicon and Linux x64. Unpack one and run `penumbra.exe` on Windows or `./penumbra` on macOS and Linux. Each package contains the docs, the example projects and the content library, and a README that says what the system needs.

The macOS and Linux builds are new and less tested than the Windows one. The macOS build is not signed: its README says how to allow it.

New since 0.9.0:

- Simulations: seven ship as components in the add dialog's new simulations tab, each with presets that write their values into its params, so a preset can be picked and then tuned.
  - `sand`, `growth` (frost, coral, lightning, lichen), `chladni` (plates that sound), `waves` (from rain on a pool to a double slit), `reaction-diffusion`, `physarum` and `lenia`.
  - Each comes with an example and a section in `docs/simulations.md`. Pause holds a simulation's picture without running its steps.
- Components:
  - A component declares params of its own with `@param` lines in its `params` text. An inner node's param follows one, written `parent.<name>`.
  - `@preset` lines give the values that choosing an entry writes, in one undo step. `@reset` lines restart a component's loops when a param changes; the new `reset` param does it from a key.
  - A component turns red, with a tooltip naming the node, when a node inside it is in error at any depth. `node.list` with `problems` lists such nodes.
- Shaders:
  - `tex.shader` runs several passes per cook with `iterations`, takes up to 8 inputs, writes 32-bit floats with format `32f`, and has a `bypass`.
  - `tex.feedback` has `repeat`: every node on its loop runs that many times per frame.
  - A shader the compiler refuses shows the compiler's lines on its node.
- New nodes:
  - Serial, UDP and TCP nodes for lines, JSON or bytes as channels and tables (`docs/serial-udp-tcp.md`).
  - `tex.screen_in` captures a monitor or a window (Windows).
  - `tex.scatter` adds points from a texture into a picture. `tex.erode` and `tex.dilate`.
  - `chan.audio_pitch` tells which note a voice, a whistle or an instrument sounds.
  - `chan.dropdown` picks one entry from a list. `chan.feedback` lets a binding close a loop a frame later.
  - `vol.obstacle` is a still sphere or box the smoke flows around.
- Extended nodes:
  - `script.python` reads pictures and puts out pictures and meshes, as NumPy arrays.
  - `chan.from_tex` measures sum, min, max and coverage exactly, names its channels and reads the texels of a small picture as channels.
  - `chan.math` gains sin, cos, tan, atan2, abs, ceil and round. `chan.from_table` picks a row by `key`.
- Projects:
  - A wire or binding in `project.ron` that would close a loop is left out with a message that walks the loop, instead of the whole project opening empty.
  - A folder without a project asks before penumbra makes one.
  - A headless run leaves the project folder as it found it unless the script saves.
- Editor: the add dialog has tabs for mapping and simulations and takes TouchDesigner's names for nodes. Table cells show four significant digits.
- Sturdier against bad input and failing devices: cameras, OSC, DMX, audio, video decoding, recording, FBX and model files. A panicking node waits for a change instead of failing every frame.
- macOS: the script node finds Python on `PATH` and runs scripts, the process statistics are native, and the show clock and DMX output keep time while every core is busy.

Changes that can touch your projects:

- `tex.raymarch` is now `tex.render_volume`, and the OSC, MIDI and data senders are `out.osc`, `out.midi` and `out.data`. Projects with the old names open, and the next save writes the new ones.
- A component's `expose` param is gone: `@expose` lines in its `params` text replace it.
