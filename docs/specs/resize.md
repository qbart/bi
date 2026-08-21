# Resizing a pane

`Ctrl-W +` moves a divider one cell. `:resize` says how far in one go, in
whichever of the three ways is shortest to type.

## Status

**Built.**

```
:resize 30        the width, in cells
:resize 30y       the height
:resize +3        three cells wider    (a bare `+` is one)
:resize -3        three narrower
:resize +3,-3     wider and shorter
:resize 1:2       divide the split one to two
:resize 1:2y      the same, vertically
:resize 1:2,1:2   both axes
:resize 1:2:1     three panes, three shares
```

## Where the parsing lives

`src/resize.rs`, in the library, with no layout in reach — the same split
`substitute.rs` and `case.rs` make. A grammar with its own tests is a grammar
you can be sure of before anything on screen has moved.

## Naming the axis

**A `y` suffix names it; otherwise a comma decides by position** — across
first, then down.

The suffix wins over the position, so `:resize 30y,10` is a height and then a
width. Nobody types the pair that way round, and it is supported anyway,
because a letter that is read and then ignored is worse than one that is
refused. That costs a second pass in the parser: which axis a part gets cannot
be known until every part has been read, and placing them in written order puts
both of `30y,10` on `y` and calls it a mistake. Naming the same axis twice
*is* refused, since it says nothing.

## What it acts on

The deepest ancestor split that runs along the axis — the divider you would be
pushing with `Ctrl-W +`, and the same target `Layout::resize` already used.
There is no separate rule to learn.

`:resize 30` is a **delta in disguise**: the pane's current extent is
subtracted and the result goes through the same path as `+3`, which is what
makes one floor, one clamp and one "no room" message serve all three forms. The
extent is the one the frontend last reported, so thirty means thirty cells of
*text* — a number you can count on screen rather than one that quietly includes
a border bi did not draw.

## Ratios

`1:2` sets the weights of that split's children directly. `Child.weight` was
already how the layout divides a rect, so this is the one form that writes what
the tree stores rather than nudging it.

**One term per child, or it is a message.** `1:2` on a three-way split is
refused with the count it wanted, because there is no non-arbitrary rule for
which pane the missing term belonged to. `1:2:1` is how you say it.

Shares are **whole numbers**, normalised when applied — so `20:40` and `1:2`
are the same thing and neither has to be written in lowest terms. `1.5:2` is
refused rather than read as the `3:4` it means: a pane is not divisible, and
someone typing the first meant the second. A share of `0` is refused too, being
a pane of nothing.

## Both axes are attempted

`:resize +6,+3` widens the pane even when there is no horizontal divider to
make it taller, and reports only the half that could not move. A layout where
one axis can give and the other cannot should give.

## What cannot be resized

A pane with no divider along that axis — the only window, or two side-by-side
panes asked to change height. Both say so rather than doing nothing quietly.
The floor is the frontend's `Chrome::min`, so a resize that would make a
neighbour smaller than a pane can be is refused whole rather than clamped to
something nobody asked for.

## Tests

- Each form moves the divider, and the signed forms undo each other.
- `:resize 30` lands on 30.
- A ratio divides the split, read off *both* panes rather than one — a split
  leaves the focus on the new window, which is the second child.
- `20:40` is `1:2`.
- The wrong number of shares says how many it wanted; no split to divide says
  that instead.
- `+6,+3` where only the width can move: the width moves, and only the height
  is reported.
- A malformed line changes nothing.
