# Spout

Spout shares pictures between programs on the same Windows machine, directly on the graphics
card: no copy through the CPU, no compression, practically no delay. TouchDesigner, Resolume,
OBS (with its Spout plugin), Unity, Unreal, vvvv, Notch and MadMapper speak it. Use it to send
penumbra's picture into OBS for streaming, to take clips from Resolume into a penumbra mapping,
or to run two penumbra instances side by side.

Spout is Windows only, and both programs must run on the same graphics card (see
[Laptops](#laptops-with-two-graphics-chips)). Between machines, use [NDI](ndi.md).

## Sending: `out.spout`

Add an `out.spout` (Tab > system > spout) and connect the picture. From its first cook it is
listed as a sender; other programs find it by its name.

- `name`: what receivers see. Empty: `penumbra` and the node's name, such as `penumbra spout1`.
  ASCII only (Spout names are not Unicode). If another program already sends under that name,
  the node sends as `name_1` and warns.
- `format`: `8bit` (BGRA, what every Spout program takes) or `16bit_float` (finer steps and
  values above 1, for receivers that take it).
- `alpha`: `straight` (most programs), `premultiplied`, or `opaque` (composited over black,
  alpha 1).

A frame is sent whenever the input changes; receivers keep showing the last one meanwhile.
The sender disappears when the node is deleted or its project closed.

## Receiving: `tex.spout_in`

Add a `tex.spout_in` and pick the sender in the **...** menu next to `sender`, which lists the
senders running now with their size and program. Empty `sender` takes the active one: the
sender picked last in a Spout program, else the first. The texture has the sender's size and
follows it when it changes.

`alpha` says how the sender's transparency is meant (`straight`, `premultiplied`) or ignores it
(`opaque`, for senders whose alpha is garbage). The picture stays live while the show is
paused, like a camera's.

`spout.senders` on the control channel lists the senders, penumbra's own included, and the one
an empty `sender` would take.

## Colour

penumbra works in linear light with premultiplied alpha; Spout programs exchange
display-ready colour (sRGB). Both nodes convert, so a picture looks the same on both sides. A
`16bit_float` sender carries the same sRGB-encoded values, only with more precision and values
above 1 kept.

## Laptops with two graphics chips

On a laptop with an integrated chip and an NVIDIA chip, both programs must run on the NVIDIA one,
or the receiver sees black or nothing. Set it per program in the NVIDIA Control Panel (Manage 3D
settings > Program settings) or in Windows (Settings > System > Display > Graphics).

## How it works

A sender is a name in a list in shared memory plus a Direct3D 11 texture with a shared handle;
receivers open the handle on their own device and copy from it. penumbra renders with DirectX
12 and reaches the Direct3D 11 texture through Direct3D 11 on 12 on its own GPU queue: one GPU
copy and one conversion pass per frame, and the cook thread never waits for the GPU. A receiver
holds the texture only while it copies; if it holds it longer than 4 ms, the sender skips that
frame rather than stall the show.
