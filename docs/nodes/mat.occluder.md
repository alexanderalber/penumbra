## A stand-in for a real object

A material that draws nothing, yet hides what lies behind it and casts shadows. Put it on a model
of a real object that stands in front of a projection surface (a column, a box, a statue): in a `tex.render` from the projector's point of view, the
content behind the object is cut out where the object blocks it, so the projector does not paint
the wall's picture onto the object.

Occluders draw before everything else and write only depth, no colour. Assign it with
`geo.assign_material` like any other material.
