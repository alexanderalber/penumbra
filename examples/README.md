# Examples

Small projects, each showing one side of penumbra. Open one with the folder button at the left
end of the status bar: **Examples**, then the example's name. Then click the node named below
to see its picture in the viewer.

The menu opens a copy of the example in penumbra's data folder (`%LOCALAPPDATA%\penumbra\examples`),
so this folder stays as it is. The copy is made afresh on every open (the menu, or the start
that reopens the last project), so changes last until the example is opened again; choosing
the open example in the menu starts it over. To keep your changes, use **Save copy as** in the
same menu. (**Open project** on the example's folder here opens it in place, and penumbra then
saves into it.)

| example | shows | look at | needs |
|---|---|---|---|
| [curl-noise](curl-noise/README.md) | 65 536 sticks drifting through a flow field, simulated on the GPU and drawn instanced | `/out` | |
| [feedback](feedback/README.md) | a loop that feeds each frame back into the next: trails, zoom, drifting colour | `/show` | |
| [audio-reactive](audio-reactive/README.md) | sound turned into channels that move the picture: level, kick, bar | `/show` | a microphone |
| [blob-tracking](blob-tracking/README.md) | finding and following spots of light, with ids that stay | `/view`, `/track` | |
| [python-script](python-script/README.md) | a Python script that keeps state: a bouncing ball | `/view` | Python 3.12 or later |
| [channels-and-pictures](channels-and-pictures/README.md) | a picture turned into channels and back | `/view` | |
| [projection-room](projection-room/README.md) | a 3D room lit by a projector, lamps and its surroundings | `/view`, `/through_beamer` | |
| [fluid](fluid/README.md) | a fluid stirred by motion: optical flow and blobs push it, the picture colours it | `/view` | |
| [synthwave](synthwave/README.md) | a retro flight: the Hikyaku courier glider from OBJ files over an instanced city, wireframe grids, a noise nebula in warm and cold palettes | `/out` | |
| [projection-onto-objects](projection-onto-objects/README.md) | a relief lit by two projectors from one camera scan: colours and contours by height, a moving sun, tide and snow line; the steps to scan your own object | `/view`, `/out_a` | (to light a real object: two projectors and a camera) |
