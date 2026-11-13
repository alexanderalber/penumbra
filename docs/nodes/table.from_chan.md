## What it is for

Some nodes take a table that says where things are: `physics.bodies` places its bodies by one,
`geo.instance` its copies. `table.from_chan` makes such a table from channels. An LFO, a sound
level or a tracker can then move what the table places, on every frame.

The node does no maths. Scale and shift the channels before it with `chan.math`, and give them
the names the table needs with `chan.rename`: `tx`, `ty`, `tz` for a place in metres.

## The three layouts

- **`columns`:** each channel is a column, named in the first row. Channels with one value each
  give one row. Channels with many samples give a row per sample.
- **`rows`:** each channel is a row, its name in the first cell.
- **`numbered`:** the number at the end of a channel's name picks the row. `x1`, `y1`, `x2`, `y2`
  give the columns `x` and `y` and two rows. A channel without a number fills its column in
  every row: one `tz` gives every row the same height.

With `numbered`, the rows run from the lowest number to the highest. A number that no channel
has gives a row of empty cells. `keep` names a column: rows with 0 there are left out. With the
`chans` of a `tex.blob_track`, `keep` set to `id` leaves out the slots that hold no blob.

`names` off leaves out the names. Most nodes that read a table need them.

## Example

A box that an LFO moves from side to side, as the one body of a driven group:

1. `chan.lfo` with `name` set to `tx`.
2. `chan.constant` with `name0` set to `tz` and `value0` 0.5.
3. `chan.merge` of the two.
4. `table.from_chan`. Its table: `tx,tz` and one row such as `0.7071,0.5`.
5. `physics.bodies` with `mode` set to `driven`, into a `physics.rigid_bodies`.

The page of [physics.rigid_bodies](../reference/nodes.md#physicsrigid_bodies) has the same for the blobs of a
tracker.
