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
| [show](show/README.md) | a show as a list of cues: five looks with fades, stepped by the space key, a GO button on the show screen or OSC, a caption per cue from a table | `/out` | |
| [control-page](control-page/README.md) | a web page that moves the project's sliders, switches and buttons from a phone or a browser, made to be copied into your own project | `/show` | (for a phone: the same network as this computer) |
| [audio-reactive](audio-reactive/README.md) | sound turned into channels that move the picture: level, kick, bar | `/show` | a microphone |
| [beat-grid](beat-grid/README.md) | the beats and bars of a sound file, found ahead of time, beside the live beat tracker on the same music: flashes on the beat, a ring on each bar's first beat, a line over the bar | `/out` | speakers or headphones |
| [pendulum-wave](pendulum-wave/README.md) | 121 pendulums seen from above, drifting apart and back into sync, each striking a note as it passes its rest point, over a hi-hat and kick | `/out` | speakers or headphones |
| [clips](clips/README.md) | a video file looped with its sound in step, the result recorded as a movie with sound; the clip made by penumbra itself | `/clip` | speakers or headphones |
| [blob-tracking](blob-tracking/README.md) | finding and following spots of light, with ids that stay | `/view`, `/track` | |
| [silhouette-wake](silhouette-wake/README.md) | a person in front of a wall, from a depth camera's mask (drawn by a shader here): stripes run across the wall without stopping, and each pixel counts them from the moment the person left it, so they bend and tear into the person's old outlines | `/out` | (for a real person: a depth camera that sends its mask through Spout or NDI) |
| [python-script](python-script/README.md) | a Python script that keeps state: a bouncing ball | `/view` | Python 3.12 or later |
| [python-pictures](python-pictures/README.md) | Python scripts that draw a picture and build a mesh with numpy: rings on a wavy sheet | `/render` | Python 3.12 or later with numpy |
| [projection-room](projection-room/README.md) | a 3D room lit by a projector, lamps and its surroundings | `/view`, `/through_beamer` | |
| [smoke](smoke/README.md) | smoke in a box in space from three emitters: a ball blowing sideways, a moving ball, a flat box; hot smoke glows in colours of its own | `/out` | |
| [sand](sand/README.md) | falling sand from the library: grains rain on a word, pile up and pour off its edges; ten scenes from an hourglass to a board of pins, chosen on one param, then tuned | `/out` | |
| [growth](growth/README.md) | slow growth from the library: frost grows over a word and melts where a moving blob passes, coral grows around it, lichen spreads; eleven presets from window frost to a Lichtenberg figure, chosen on one param, then tuned | `/out` | |
| [chladni](chladni/README.md) | Chladni figures from the library: sand on a vibrating plate gathers on its still lines, and a melody, heard by a pitch tracker, changes the figure every 2 s; eight presets on a square, a round and a guitar-shaped plate, chosen on one param, then tuned | `/out` | (for your own voice: a microphone) |
| [raymarch](raymarch/README.md) | three raymarched looks that follow music: a tunnel, melting shapes and a fractal, chosen with one slider; a built-in beat drives them | `/out` | (for real music: a microphone or line input) |
| [fluid](fluid/README.md) | a fluid stirred by motion: optical flow and blobs push it, the picture colours it | `/view` | |
| [music-synthwave](music-synthwave/README.md) | a complete show on one song: 15 elements on two stages, a synthwave world that rolls up into a tunnel at the drop after the break, the arrangement written by one command from the beats and sections found in the file; the song is by Krzna and not under penumbra's licenses | `/out` | speakers or headphones |
| [projection-onto-objects](projection-onto-objects/README.md) | a relief lit by two projectors from one camera scan: colours and contours by height, a moving sun, tide and snow line; the steps to scan your own object | `/view`, `/out_a` | (to light a real object: two projectors and a camera) |
| [projector-wall](projector-wall/README.md) | three projectors as one picture on a wall: each projector's share, blend weight, flat field and colour match, measured on a real wall; the steps to measure your own | `/wall`, `/unblended` | (to light a real wall: three projectors and a camera) |
