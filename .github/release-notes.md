## penumbra 0.11.0

Builds for Windows x64, macOS on Apple silicon and Linux x64. Unpack one and run `penumbra.exe` on Windows or `./penumbra` on macOS and Linux. Each package contains the docs, the example projects and the content library, and a README that says what the system needs.

The macOS and Linux builds are a preview and less tested than the Windows one. They have no cameras yet (`tex.video_in`, and with it scanning and tracking from a live camera), no screen capture, no Spout and no SpaceMouse: those work on Windows only so far. The macOS build is not signed: its README says how to allow it. There is no build for Intel Macs.

New since 0.10.0:

- Points: a new kind of data, points on the GPU, with a family of nodes of its own (`pts.*`, `docs/points.md`). The add dialog also finds them by TouchDesigner's POP names.
  - Make points with `pts.generate`, from pictures, meshes, channels and tables, or from point cloud files (`.ply`, `.xyz`, `.csv`, `.pts`).
  - Change them with shaders, noise, transforms, maths and random numbers, or with a program of your own.
  - `pts.particles` gives birth, ageing and death, with forces, fields and a feedback loop. `pts.trail` draws trails behind them.
  - `pts.proximity` joins near points with lines (a plexus), and `pts.neighbor` counts and averages neighbours (flocks).
  - `geo.from_pts` draws points as copies of a model, as sprites (also soft and added) or as lines.
- Rigid bodies: `physics.rigid_bodies` drops, piles and throws copies of models. Several groups of bodies, each with its own model, shape and material, can be driven by a table or stand still. `physics.field` pulls, blows, whirls and kicks them. The solver also draws its bodies as a mask and a velocity picture.
- Liquid: `tex.liquid` is water seen from the side. It pours from emitters, falls, pools on obstacles and drains through open edges. It also holds sand that heaps and snow that piles up. Moving obstacles push it, the rigid bodies too.
- New examples: `particles` (sparks rising from a word), `rigid-bodies`, `liquid` and `liquid-bodies`.
- Python: when the search finds no Python, a script node offers a button to choose one by hand. The choice holds for every project.
- Agents: the MCP server explains the shape of each command's arguments, and an unknown command names the close ones.
- Fixed: the person in `silhouette-wake` walks: feet that stay put, knees that bend forward.

Changes that can touch your projects:

- The examples `show`, `clips`, `control-page`, `projection-onto-objects` and `projector-wall` are gone. `docs/scanning.md` still walks through a relief from scan to picture, and `docs/helpers.md` through blending and matching projectors. A `chan.http_in` serves its built-in control page without one.
- A `tex.shader` waits for a wired input whose node has no picture yet, instead of drawing one frame from an empty picture. A node that failed counts as an empty picture, and so does a `tex.feedback` with nothing at `initial` that waits for the shader itself.
