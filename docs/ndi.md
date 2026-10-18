# NDI

NDI® sends pictures and sound between programs over the network: OBS, vMix, Resolume,
TouchDesigner, many PTZ cameras, the free NDI Tools (Studio Monitor, Test Patterns, Screen
Capture) and another penumbra on a second machine. Use it to show penumbra's picture on a
machine across the room, to take a camera or a presenter's laptop into a mapping, or to let a
streaming PC pick up the show. On one machine, [Spout](spout.md) is faster and costs nothing;
NDI works between machines (and on one too).

NDI® is a registered trademark of Vizrt NDI AB. More about NDI and its tools:
[ndi.video](https://ndi.video/).

## What you need

NDI is not part of penumbra. Install **NDI Tools** ([ndi.video/tools](https://ndi.video/tools/))
or only the **NDI Runtime** ([ndi.link/NDIRedistV6](http://ndi.link/NDIRedistV6)) on every
machine that sends or receives. penumbra finds it through the `NDI_RUNTIME_DIR_V6` variable
the installer sets (or in NDI's usual folders); without it the two NDI nodes fail with a
message saying where to get it, and everything else works as before. `ndi.sources` on the
control channel reports the runtime it uses.

The machines must reach each other on the network. The first time a node sends or receives,
Windows may ask whether penumbra may use the network: allow it for private networks. Sources
are found by mDNS on the local network; a discovery server, if your network uses one, is set
up with NDI Access Manager and used by penumbra as well.

## Sending: `out.ndi`

Add an `out.ndi` (Tab > outputs > ndi) and connect the picture. From its first cook it is a
source on the network; other programs list it as `MACHINE (name)`.

- `name`: what receivers see after the machine's name. Empty: `penumbra` and the node's name,
  such as `penumbra ndi1`.
- `alpha`: `opaque` (the default: composited over black), `straight` (the receivers get the
  transparency, as NDI defines it) or `premultiplied`. Sending alpha costs about twice the CPU
  and some 10 ms of latency (measured at 1080p60), and most receivers ignore it, so leave it
  `opaque` unless a receiver uses the transparency.

A frame is sent whenever the input changes, announced at the show rate; receivers keep the last
one meanwhile. The source disappears when the node is deleted or its project closed.

## Receiving: `tex.ndi_in`

Add a `tex.ndi_in` and pick the source in the **...** menu next to `source`, which lists the
sources on the network and fills in as they answer (give it a second after the first opening).
`source` also takes a part of the name, such as `OBS`; empty takes the first source. The
texture has the source's size and follows it when it changes.

- `bandwidth`: `full` (the stream as sent) or `low` (a smaller stream the sender offers beside
  it, for previews and multiviews on a busy network).
- `alpha`: how the source's transparency is taken: `straight` (NDI's), `premultiplied`, or
  `opaque` to ignore it.

The picture stays live while the show is paused, like a camera's.

The second output, `audio`, carries the source's sound: one channel per source channel (`ch1`,
`ch2`, ...) at its sample rate, what arrived since the last cook, as `chan.audio_in` delivers a
sound card's. Feed it into a `chan.audio_analysis` to react to a DJ's NDI feed. Sound is only
kept while something reads the output.

`ndi.sources` on the control channel lists the sources (name and address), penumbra's own
included.

## Colour

penumbra works in linear light with premultiplied alpha; NDI carries display-ready colour as
Y'CbCr 4:2:2 (video range, BT.601 for SD, BT.709 for HD, BT.2020 above 1080p) and straight
alpha. Both nodes convert on the GPU, so a picture looks the same on both sides.

## Latency and load

Measured on one machine (Ryzen 5800X3D, RTX 3090), 1080p60, NDI 6.3:

| | CPU (of one core) | latency |
|---|---|---|
| sending without alpha | about 10 % | |
| sending and receiving without alpha | about 11 % | 2 to 5 ms (median), under 12 ms |
| sending and receiving with alpha | about 25 % | about 15 ms |

Latency is from handing a frame to NDI to receiving it, on the same machine; a gigabit network
adds little. A 1080p60 stream takes roughly 100 to 150 Mbit/s, so use wired gigabit Ethernet,
not Wi-Fi, for full streams.

## How it works

Sending: a compute pass turns the texture into UYVY (and an alpha plane), the bytes come back
from the GPU through a small ring of buffers without the cook thread waiting, and a thread of
the node's own hands them to NDI, which compresses and sends them. When every buffer is still
on its way, the frame is left out rather than stall the show. Receiving: a thread of the node's
own takes NDI's frames as they come and keeps only the newest; the node uploads it and converts
it on the GPU when it cooks.
