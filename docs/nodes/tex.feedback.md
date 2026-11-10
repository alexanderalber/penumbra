## What it is for

Normally a picture flows one way, from left to right through the chain, and every frame starts
from scratch. A feedback loop hands each frame's result back to the start of the chain, so the
next frame builds on it. That is how trails, echoes, smears and patterns that grow by themselves
come about: whatever the chain does to a frame (move it a little, dim it, shift its colour)
happens again and again, and adds up.

## Wiring it

1. Put a `tex.feedback` at the start of the chain and wire the picture to start from into
   `initial` (a black `tex.constant` is the usual choice).
2. Build the chain from the feedback's output: what should happen to the last frame each time.
3. Wire the end of the chain back into the feedback's `previous`. That wire is drawn dashed: it
   carries the previous frame, so it may run backwards without making a cycle.

Until the end of the chain has made a first frame, and while `reset` is on, `initial` is passed
on. R, the reset button or `node.reset` clear the loop the same way.

## Keeping it in check

Every step repeats every frame, so small values matter: a `tex.level` with `brightness` 0.96
lets a trail die out within a few seconds, at 1 nothing ever fades and the picture fills up. A
`tex.transform` with `scale` 1.01 zooms the trails slowly outwards. Mix new content in near the
end of the chain (a `tex.composite` with the content as `in1`), or the loop has nothing to
work on but `initial`.

With `persist` on (the default), the loop's last frame goes into the state snapshots, so a
restart carries on from where it stopped.

## Several runs per frame

`repeat` runs the loop several times per frame. With `repeat` 3, every node between the feedback
and the end wired into `previous` cooks three times, each run starting from the run before. A
trail then grows three times as fast and looks as it does at 1. Nodes outside the loop cook
once: what feeds the loop from outside, and what reads its result. Another feedback whose
`previous` is wired straight from a node of the loop, and which feeds the loop, runs with it. A
feedback that reads the loop through other nodes stays outside and cooks once. The clock stands
still between runs. The loop costs its time three times. Details for simulations and shaders:
[A step of several nodes](../shaders.md#a-step-of-several-nodes).

## Example

The `feedback` example project: text drawn over a loop that zooms, turns, dims and shifts the hue
of the last frame a little, so the text leaves trails that drift outwards and change colour.
