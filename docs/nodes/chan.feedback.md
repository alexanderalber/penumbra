## What it is for

Wires and bindings may not form a loop: every node needs its sources cooked before it. A
`chan.feedback` lets channels come round anyway, one frame late. Its `previous` input sees what
its source made in the frame before. The wire into `previous` may run backwards, as the one into
a `tex.feedback`'s `previous` does, and is drawn dashed.

The usual case is a measurement that acts back on what it measures. A `chan.from_tex` counts
what is alive in a simulation, and the simulation's `tex.feedback` should restart when the count
falls to 0. Binding `reset` to the `chan.from_tex` is refused, because the count depends on the
loop. Bound to a `chan.feedback` of the count, the loop restarts a frame after the count fell.

## Wiring it

1. Wire the channels to send back (the end of the measurement) into `previous`.
2. Bind the param in the loop to the `chan.feedback`.

The output has the input's channels and names. In the first frame, and right after a reset, it
has no channels yet. A param bound to it then keeps its stored value for that frame.
