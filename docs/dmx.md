# DMX

`out.dmx` (Tab > outputs > dmx) sends channels to lights: through an Enttec Open DMX USB on
this machine, or over the network as Art-Net, sACN or DDP.

## Setting up

1. Add an `out.dmx` and connect channels to it. The first channel goes to slot `address`, the
   next to the one after, and so on, in the order of the channels. A `chan.merge` puts several
   sources into one list; `chan.select` and `chan.rename` pick and order them.
2. With `values` on `unit`, 0 to 1 becomes 0 to 255 (most dimmers and colours); with `byte` the
   values are slots as they are. Anything below or above is clamped.
3. Pick the `protocol`:
   - `open_dmx`: the Enttec Open DMX USB. `port` empty takes the first FTDI FT232R plugged in
     (`dmx.ports` lists the serial ports); type `COM5` or `/dev/ttyUSB0` when there are several.
     One universe, so `universe` does not count.
   - `art_net`: to an Art-Net node at `host`, or as a broadcast while `host` is empty.
     `universe` is the port address as Art-Net counts, from 0.
   - `sacn`: to each universe's multicast group while `host` is empty, else to that host.
     Universes from 1.
   - `ddp`: to a pixel controller such as WLED at `host` (needed). No universes; `address` is the
     byte the first channel goes to, three channels per RGB pixel.

   Params the protocol does not use are greyed, and their help says why. `host` takes an IP
   address or `localhost`, optionally with `:port`; names are not looked up.

More than 512 channels spill into the next universes (Art-Net, sACN, up to 64 universes);
the Open DMX USB leaves them out and says so on the node.

## How it sends

Each target (a port, an address) has a thread of its own that sends the newest frame `rate`
times a second (default 30), also while nothing changes and while the show is paused: the Open
DMX USB has no memory of its own, and sACN receivers give up on a source that falls silent.
Removing the node stops what it sent; lights then do what they do on lost signal (most hold
the last look).

Several `out.dmx` nodes may send to the same target, each to its own slots: one node per
fixture, say. Where two write the same slot, the one added later wins, and both show a warning.

When the Open DMX USB is pulled or another program holds its port, the node shows the error
and the port is tried again every second.

## The Open DMX USB

It has no processor of its own: penumbra makes the DMX signal itself through the FTDI chip,
break and all. A full universe takes 22.6 ms on the wire plus about 5 ms for the break and a
margin, so it manages about 35 frames a second; 30 is a good rate. `dmx.status` shows what it
does: the rate measured, the time between frames (min, mean, max) and how long break and write
took. On Windows it uses FTDI's virtual COM port driver, which Windows installs by itself.

## Commands

- `dmx.ports`: the serial ports, the FT232R ones (`open_dmx: true`) first.
- `dmx.status`: every output: target, writing nodes, device, open, error, frames, rates and
  timings.
- `dmx.config {blackout: true}` sends zeros on every output until `{blackout: false}`;
  `{enabled: false}` stops sending altogether and closes the port.
