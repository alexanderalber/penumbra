# Serial, UDP and TCP

`chan.data_in` (Tab > chan > data_in) reads numbers and text from a serial port, UDP or TCP: an
Arduino on a USB cable, a sensor on the network, another program. Each name becomes a channel to
bind params to, and the last messages become a table. `chan.data_out` (Tab > chan > data_out)
goes the other way: it sends channels and table rows. For OSC use `chan.osc_in` and
`chan.osc_out`; for DMX see [DMX](dmx.md).

## Reading

1. Add a `chan.data_in` and pick the `transport`:
   - `udp`: datagrams sent to `port` (default 7000).
   - `tcp_listen`: penumbra is the server on `port`; programs connect and send. Up to 16 clients at once.
   - `tcp_connect`: penumbra connects to the server at `host` and `port`, and connects again when it drops.
   - `serial`: the port in `serial` (`COM3`, `/dev/ttyACM0`) at `baud`, 8 data bits, no parity, 1 stop bit.
     Empty takes the only port plugged in. `net.ports` lists them.
2. Pick the `format`, see below. One message ends at `delimiter` (`\n` by default; a `\r` before it is ignored).
   Over UDP each datagram is one message, and lines or JSON in one datagram are split at the delimiter too.
3. Bind a param to a channel of the node.

`udp` and `tcp_listen` take only programs on this machine until `lan` is on. Windows may then ask
once whether penumbra may use the network.

## Formats

- **lines:** `1023,512,0` makes `chan0`, `chan1`, `chan2`. `x=1 y:2` and `temp: 21.5` make named
  channels. `names` renames the unnamed fields in order: `x y z` makes `1,2,3` into x, y and z, and `-`
  leaves a field out. A line splits at commas, semicolons or tabs, else at spaces (`split` picks one).
  A field that is not a number shows only in the table.
- **json:** one value per message. `{"temp": 21.5, "acc": {"x": 1}, "xy": [0.5, 0.2]}` makes `acc_x`,
  `temp`, `xy_0` and `xy_1` (keys come in alphabetical order). `true` is 1, `false` 0. A top-level array
  `[1, 2]` makes `chan0` and `chan1`, named by `names` like lines.
- **bytes:** each byte a channel, 0 to 255. On a stream, a message ends at the delimiter (empty for none),
  else after `length` bytes, else with each read.

A channel holds its latest number until a message sets it again. The `table` output has the names
as its first row, then the last `rows` messages, the newest last.

## Sending

`chan.data_out` sends its `chans` input as one message and every row of its `table` input as one
more, at most `rate` times a second.

- **lines:** `0.5,1`, or `x:0.5,y:1` with `names` on; `separator` picks the comma. A table row is its cells joined.
- **json:** `{"x":0.5,"y":1}`; a table row is an array, `["go",1]`.
- **bytes:** each channel one byte, rounded and held to 0 to 255. A table row's number cells are bytes and
  its other cells their text, so a row `AT`, `13`, `10` sends `AT\r\n`.

`send` at `changes` sends when a message changed. Over UDP everything goes again once a second, so a
lost datagram does not leave a value wrong. Over TCP and serial nothing is resent: a client that
connects, and a connection opened again, get the latest messages at once. `delimiter` ends each
message on a stream; UDP sends each message as one datagram without it. `tcp_connect` connects to
`host`; `tcp_listen` sends to every client connected.

## One device both ways

A `chan.data_in` and a `chan.data_out` with the same serial port, or the same `tcp_connect` host and
port, share one connection. A request the out node sends gets its answer in the in node. A serial
port opens at the `baud` of the node that opened it first; the other node warns when its own `baud`
differs.

## When something goes wrong

- **A message that does not fit** (JSON that does not parse, longer than `max` bytes) is dropped
  and counted; the node warns with the last reason.
- **A peer that goes away** (a TCP server stopping, a USB cable pulled) leaves the channels at
  their last values with a warning. penumbra tries again after 0.5 s, then waits twice as long each
  time, up to 5 s. A port another program holds is tried again every second.
- **Caps:** 1024 channels and 256 table columns per node, 1024 fields per message, 4096 characters
  per text cell. Names beyond are left out and counted.

## Commands

- `net.ports`: the serial ports of this machine.
- `net.status`: every link with its readers and writers, open or not, the bound address (the port
  that `port` 0 got), who is connected, bytes and messages, drops with the last reason.

Serial is written to the same tests as UDP and TCP (framing, parsing, reconnect logic), but has not
been tried with a real device yet.

## Trying it without a device

Set a `chan.data_in` to its defaults (UDP, port 7000) and send it a line from Python:

```python
import socket
socket.socket(socket.AF_INET, socket.SOCK_DGRAM).sendto(b"x=0.5 y=0.25\n", ("127.0.0.1", 7000))
```

The node then has channels `x` and `y`. An Arduino sketch that prints `Serial.println(analogRead(A0));`
at `Serial.begin(9600)` gives `chan0` with `transport` `serial`.
