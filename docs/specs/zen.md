# Zen

`:zen` strips the chrome: the sign column, the line numbers and every
window's status row go; the text keeps its place and the command line stays —
you still need to type `:zen` again, and a message still needs somewhere to
land. Toggling it back restores exactly what was there.

## Status

**Built.**

## What it is

A session toggle — `Session::zen`, flipped by `:zen` — exactly the shape of
`:ts` and its `ts_marks`: a way of *looking* rather than a fact about any
file, which is why it is not an option. Options resolve per buffer, and a
mode that half the windows were in would not be a mode.

The core holds the bool; what to stop drawing is the frontend's call, because
the chrome was always the frontend's. The TUI reads it in three places: the
sign column and number column render at width zero, and the per-window status
row is not reserved — the text gets the row back. Decorations are untouched:
indent guides, diagnostics on the text, search highlights and the cursor line
are about the text, not around it.

The gates the options already hold keep holding: `:zen` hides the gutter but
does not pretend `gutter = 0`, so leaving zen brings the sign column back
without anything having to remember what it was.

## Deliberately not here

Centring the text, widening margins, hiding the file tree, a width option.
Zen removes what bi added around the text; it does not add furniture of its
own.
