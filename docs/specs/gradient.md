# Gradient editor

A gradient is a list of stops, each a position along `0..1` and a
colour. Tuning one by hand means editing hex digits and fractions while
imagining the ramp. `:set editor gradient` with the cursor in the list
draws the ramp beside the text with a triangle under each stop, and the
keys slide the stops and hand their colours to the colour picker
(`color-picker.md`). The file stays the file; the picture is a view of it.

## Status

**Built.**

## What it looks like

```
┌────────────────────────────────────────────────────────┐
│ ████████████▓▓▓▓▓▓▓▓▒▒▒▒▒▒░░░░░░                        │   the ramp, 512 by 64, over a checkerboard
└────────────────────────────────────────────────────────┘
  ▲            ▲              ▲                        ▲     one triangle per stop, filled its colour,
  0            0.25           0.5                      1     the selected one white
```

Three windows: the **source**, the **bar** split to its right, the
**form** at the edge, focus on the bar. The ramp is linear in sRGB
between neighbouring stops and flat outside them, which is what CSS and
every engine's default do.

## The keys

```
Tab  Shift-Tab   the next, previous stop, wrapping; counts multiply    gg  G   first, last
h  l  ← →        the stop earlier, later by one step; counts multiply
0  $             the stop to 0, to 1
a                a stop halfway to the next one, in the ramp's colour there, selected
x                delete the stop; a gradient keeps two
c                the colour picker over this stop's colour
yh  yf  yr       yank this stop's colour as hex, floats, bytes
Enter            the stop on the ex line: `:tool gradient t 0.5`
u  Ctrl-R        undo, redo — the source buffer's
Esc              back to the source, the bar stays
:                the ex line
```

A step is `0.05`; `:tool gradient step 0.01` for finer. `t` is clamped to
`0..1` and nothing else: **stops sort by `t`**, and one slid past its
neighbour changes places with it in the list, in the text, and stays
selected. One slid onto a neighbour's exact position passes it in the
direction it was going, so `l` at the clamp still changes places. The
list an engine reads is always sorted, and the editor never asks you to
keep it so. From the last stop, `a` adds halfway back to the previous
one.

`c` opens the picker with the bar as the window to come back to: `Esc`
or `:q` in the picker lands on the bar, and while the picker is open
every step it takes redraws the ramp, since both are views of one buffer.
Every key that changes something is one edit of the source buffer and one
undo step of it.

**Ex forms:**

```
:tool gradient stop 2          select the second; reports without a value
:tool gradient t 0.5           move the selected stop; sorted after
:tool gradient color #ff8800   its colour; six or eight digits as the literal has
:tool gradient step 0.05
:tool gradient add [t]         a stop at `t`, or halfway
:tool gradient delete
:tool gradient pick            the picker, as `c`
:tool gradient yank hex|floats|rgb
```

`:tool gradient` alone lists these. **`:w` and `:q` are the code's**, and
closing any one of the three windows closes the other two, as for the
curve.

## Finding the literal

A bracket scan, the curve's: from the cursor, out to the innermost group
whose direct children are groups each holding **one number and one
colour** — a colour being `#` and six or eight hex digits, quoted or not:

```json
[[0, "#000000ff"], [0.5, "#ff8800ff"], [1, "#ffffffff"]]
```
```cpp
{ {0.0f, "#000000"}, {0.5f, "#ff8800"}, {1.0f, "#ffffff"} }
```

Two entries per stop and no more: a layout is not needed, because a
number is a position and a colour is a colour. A cursor in no such group
says `no gradient under the cursor`.

**Writing back** rewrites the number token as the curve does, keeping its
suffix and decimals; a colour is rewritten by the picker's rule, digits
only, case kept. When a move changes the order, every stop's text is
written into the slot its rank now has — the brackets, separators and
lines around them stay where they were, so a one-per-line list stays one
per line. `a` copies the selected stop's text, its number written with the
decimals the midpoint needs and never fewer than the copy had; `x`
removes the group and the separator after it, or before it for the last.

**The anchor** is the outer group's open bracket, re-read after every
buffer change and re-found from the source's cursor, or the property
view's path, when it is lost; otherwise `gradient lost` and a blank bar.

## In the property view

A `gradient` field (`bi-format.md`) draws as sixteen bricks sampled
across the ramp and the count of stops — `▆▆▆▆▆▆▆▆▆▆▆▆▆▆▆▆  3 stops` —
and `Enter` on it opens the bar over the value, the view as the source,
by the curve's arrangement: an inherited gradient is written into the
instance first. `:set editor gradient` on the view does the same. `dd`
puts the default back.

## The model

`src/gradient.rs`, with no editor in it:

```rust
pub struct Stop { pub t: f32, pub color: Rgba }
pub struct Gradient { pub stops: Vec<Stop> }            // sorted by t
pub struct Literal { open, close, stops: Vec<StopSpan> }
pub struct StopSpan { start, end, number: Token, color: color_picker::Literal }

pub fn find(text: &str, cursor: usize) -> Option<Literal>;
pub fn read(text: &str, lit: &Literal) -> Gradient;
pub fn eval(g: &Gradient, t: f32) -> Rgba;
pub fn sorted_order(g: &Gradient) -> Vec<usize>;        // slot k → which stop belongs there
pub fn linear() -> Gradient;                            // black to white
pub fn stop_text(text: &str, like: &StopSpan, t: f32, color: Rgba, step: f32) -> String;
pub fn render(g: &Gradient, selected: usize) -> (u32, u32, Vec<u8>);
```

`Tool::Gradient(GradientTool)` beside the curve and the picker: source,
bar, form, buffer, anchor, path, the selected stop, the step, the
gradient and its literal as last read. The form is a readout plus the
step:

```
Stop      2               read-only
T         0.500           read-only
Step      ━●━━━━━━━━   0.050
```

The status row says `stop 2 of 3  t 0.500  #ff8800ff` and `GRADIENT`.

## Tests

- `find` on the JSON and C++ samples returns three stops with the number
  and colour spans; a cursor outside says nothing; a list of curve points
  is not a gradient.
- `eval` at a stop is its colour, halfway is the mix, outside is flat.
- `sorted_order` reads an unsorted list in order; `eval` does too.
- `render` is 512 wide with a triangle per stop.
- Opening beside the source; `l` rewrites one number; sliding past a
  neighbour swaps the two stops' texts and keeps the moved one selected;
  `a`, `x`, `Tab`, `gg`, `G`; `c` opens the picker with the bar to come
  back to and `l` there rewrites the stop's digits and redraws the ramp;
  `yh`; `Enter` prefills; `:tool gradient t 0.5`, `color`, `add`,
  `delete`, `stop`; `:q` closes three windows.
- The property view: a `gradient` row's widget and count; `Enter` opens
  the bar; an inherited one is written first; `:bi set` takes the JSON.
