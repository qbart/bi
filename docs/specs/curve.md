# Curve editor

A tuning curve lives in code: a vector of points, each a brace group of
numbers, and the numbers are what the engine reads. Tuning it means
changing those numbers while looking at the shape they make, and a text
editor shows the numbers but not the shape. `:set editor curve` with the
cursor inside the list draws the curve beside the code, and the keys move
its points by rewriting the numbers in place. The file stays the file; the
picture is a view of it.

## Status

**Built.** Points move; tangents are read, drawn and evaluated but not
yet edited — tangent rotation and the lock toggle come next and slot into
the same keys.

## What it looks like

Three windows, the normal map's arrangement: the **source** you started
from, the **plot** split to its right, and a **form** down the right edge.
Focus lands on the plot, because that is where the keys are.

```cpp
std::vector<Point> damage = {
    {0.0f, 0.0f, 1.0f, 1.0f, false},
    {0.5f, 0.8f, 0.0f, 0.0f, true},      ← the cursor is anywhere in here
    {1.0f, 1.0f, 1.0f, 1.0f, false},
};
```

```
 1.0 ┤                    ╭────●
     │              ╭─────╯
 0.8 ┤         ╭──◉╯                    the selected point, larger, with its
     │     ╭───╯                        two tangent handles as short strokes
 0.5 ┤   ╭─╯
     │ ╭─╯
 0.0 ●─╯
     └────┬────┬────┬────┬────┬──
         0.2  0.4  0.6  0.8  1.0
```

The source's struct decides what each number means; the tool is told the
order once:

```
:tool curve layout x,y,out,in,locked      the default; `_` skips a field
```

`x` is time, `y` is value, `out` and `in` the tangents as slopes, `locked`
whether the two tangents move together. A point with fewer numbers than
the layout names has zero tangents that the tool does not touch.

**An empty list is seeded.** `:set editor curve` with the cursor in `{}`
writes the linear preset into it — `(0, 0)` with an out slope of 1 and
`(1, 1)` with slopes of 1, both locked — spelled through the layout as
`{0.0, 0.0, 1.0, 0.0, true}` and `{1.0, 1.0, 1.0, 1.0, true}`, one per
line when the brackets are on different lines. That is one undo step,
and from then on the list is a list like any other.

**The square is fixed.** The plot is the `0,0` to `1,1` square at 512
pixels a side, whatever the pane's size: a curve looks the same every
time it is opened, and zoom is the zoom every picture has. A point past
the square stretches the picture to take it in at the same scale, up to
4096 pixels a side. A grid line every tenth, the whole numbers brighter,
a label every fifth.

## The keys

On the plot, read from the normal keymap the way the tileset reads its
keys, so a rebound `j` still means down and the arrows move too — except
`Tab` and `Enter`, which nothing in the normal keymap claims for a
picture and which the plot takes as its own:

```
Tab  Shift-Tab   next, previous point, wrapping; counts multiply    gg  G   first, last
h  l  ← →        x earlier, later by one x step, a tenth; counts multiply
j  k  ↓ ↑        y down, up by one y step, a tenth; counts multiply
a                add a point halfway to the next one, on the curve, and select it
x                delete the point; the first and last stay, and a curve keeps two
u  Ctrl-R        undo, redo — the source buffer's
Enter            the point on the ex line: `:tool curve y 0.8`
Esc              back to the source, the plot stays
:                the ex line
```

`x` moves inside `0..1` and between the point's neighbours, which is the
engine's own rule for time; `y` is free. From the last point, `a` adds
halfway back to the previous one, since nothing comes after the last. A
new point takes the curve's value and slope where it is added, so `a`
never bends the curve — it gives you a handle where there was none. Its
`locked` is `true` when the layout has one.

Every key that changes something is one edit of the source buffer and one
undo step of it. `u` on the plot undoes that edit, the buffer re-parses and
the plot follows; `u` in the source does the same thing, because it is the
same history. There is no second undo stack anywhere in the tool.

**Ex forms** of the same moves, for scripts and exact values:

```
:tool curve point 3          select the third point; reports without a value
:tool curve x 0.5            move the selected point in x; clamped to its neighbours
:tool curve y 0.8            and in y
:tool curve xstep 0.1        what h and l move by; ystep the same for j and k
:tool curve layout x,y,out,in,locked
```

`:tool curve` alone lists these. A value that does not parse is refused,
naming what the field takes.

**`:w` and `:q` are the code's.** The plot is a view of the source, not a
picture of its own: `:w` from the plot or the form writes the source
buffer, and `:q` from either closes the plot and the form and puts the
cursor back in the code. `Esc` is the softer way out — focus to the
source, the tool still open, `Ctrl-W` back into it. `:set editor curve`
on a source that already has the tool focuses its plot.

## Finding the literal

A bracket scan, no tree-sitter, so it works in any language that spells a
list with brackets. From the cursor, walk out through the enclosing `{}`,
`[]` and `()` groups to the innermost one whose direct children are groups
each holding at least two numbers. Inside a child, the tokens that are
numbers, `true` or `false` are taken in the order they appear and
everything else is ignored, which is what makes these all read as the
same three points:

```cpp
{ {0.0f, 0.0f, 1.0f, 1.0f, false}, {0.5f, 0.8f, 0.0f, 0.0f, true}, ... }
```
```rust
vec![Point { x: 0.0, y: 0.0, out: 1.0, in_: 1.0, locked: false }, ...]
```
```python
[(0.0, 0.0), (0.5, 0.8), (1.0, 1.0)]
```

A number is an optional sign, digits with an optional fraction and
exponent, and an optional suffix of letters and underscores glued to it:
`0.5f`, `1e-3`, `-2.0_f32`. `0` and `1` where the layout says `locked`
count as `false` and `true`. A cursor in no such group says `no curve
under the cursor`; a group whose children disagree on their count is
still a curve — the layout is applied per point, and short points simply
lack the trailing fields.

**Writing back** rewrites tokens, never the text around them. A number
keeps its suffix and at least as many decimals as it had, and never fewer
than the step needs, so `0.5f` moved by `0.01` becomes `0.51f` and `1`
moved by `0.25` becomes `1.25`. A bool keeps its spelling, `true` or `1`.
`a` copies the point's text — its brackets, a name glued to them like
`Point { … }`, the separator after it — and puts the new numbers in, so a
one-per-line list stays one per line; `x` removes the group and the
separator after it.

**The anchor.** The tool remembers the byte offset of the outer group's
open bracket. After every change of the buffer it re-reads the group from
there. Edits the tool makes never move that bracket; edits made by hand
before it might, and then the tool looks again from the source window's
cursor, and if that finds nothing either the status says `curve lost` and
the plot goes blank until the next `:set editor curve`.

## The model

`src/curve.rs`, with no editor in it:

```rust
pub struct Layout { fields: Vec<Field> }          // Field: X | Y | Out | In | Locked | Skip
pub struct Point { pub x: f32, pub y: f32, pub out: f32, pub in_: f32, pub locked: bool }
pub struct Curve { pub points: Vec<Point> }        // sorted by x

/// Where each point's tokens sit in the text, so a move rewrites in place.
pub struct Literal { open: usize, close: usize, points: Vec<PointSpan> }
pub struct PointSpan { start: usize, end: usize, tokens: Vec<Token> }   // start reaches back over a glued name

pub fn find(text: &str, cursor: usize) -> Option<Literal>;
pub fn find_empty(text: &str, cursor: usize) -> Option<(usize, usize)>;   // an empty list's brackets
pub fn initial_text(text: &str, open: usize, close: usize, layout: &Layout) -> String;
pub fn linear() -> Curve;
pub fn read(text: &str, lit: &Literal, layout: &Layout) -> Curve;
pub fn rewrite(token: &str, value: f32, step: f32) -> String;
pub fn eval(curve: &Curve, x: f32) -> f32;         // cubic hermite between neighbours
pub fn slope(curve: &Curve, x: f32) -> f32;
pub fn point_text(text: &str, like: &PointSpan, layout: &Layout, p: Point, step: f32) -> String;
pub fn plot_range(curve: &Curve) -> ((f32, f32), (f32, f32));   // the unit square, stretched to the points
pub fn render(curve: &Curve, selected: usize) -> (u32, u32, Vec<u8>);
```

**Evaluation** is Unity's: between points `p` and `q` with `d = q.x - p.x`,
the hermite basis over `t = (x - p.x) / d` with tangents `p.out * d` and
`q.in * d`; before the first point and after the last, the curve is flat.
`locked` changes nothing in the evaluation — it is a promise about how
the tangents move, which is not yet a thing that happens.

**x stays sorted.** A point's x is clamped between its neighbours' x and
inside `0..1`, so the list the engine reads never needs sorting. Two
points may share an x; the segment between them has zero length and is
skipped.

**The picture** is drawn into RGBA at a fixed scale, 512 pixels per unit,
the unit square plus margins for the labels; the picture machinery
scrolls and zooms it like any other. The curve, every point as a dot, the
selected point larger with both tangent handles drawn the engine's way —
`normalize(1, slope) · 0.12` units from the anchor, out to the right and
in to the left. The plot's `Img` is named after the source with a
`.curve` extension so the status row says `damage.cpp.curve`, and it is
never dirty: it is derived.

## The tool in the editor

```rust
struct CurveTool {
    source: WindowId, plot: WindowId, form: WindowId,
    buffer: BufferId,
    anchor: usize,             // byte offset of the outer open bracket
    layout: Layout,
    selected: usize,
    xstep: f32, ystep: f32,    // a tenth each, by default
    seen: (u64, u64),          // buffer edits, form generation, the plot reflects
}
```

`Editor::tools` becomes a `Vec<Tool>`, an enum over the normal map and the
curve, so `sync_tools`, `close_tool`, `tool_at` and `form_home` are one
path each. After every key the sync asks each curve tool whether its
buffer's edit counter or its form's generation moved and, if so, re-finds
the literal, re-reads the curve, rebuilds the form's mirror fields and
re-renders the plot. Because a key's edit goes *into* the buffer and the
plot is rebuilt *from* the buffer, there is one direction of data flow
and nothing to keep in step by hand.

The plot's window is an image window; `run_image_action` reads the actions
above as curve moves when the image is a curve tool's plot, ahead of the
pixel scrolling a plain picture would do, exactly as it reads them as
tiles when the tileset is on. `a`, `i` and `x` arrive as the append,
insert and delete-character actions normal mode already names.

## The form

A readout and a place for exact values, the same `Form` every tool uses:

```
Point     ‹ 2 of 3 ›
X         ━━━━●━━━━━   0.500        between the neighbours
Y         ━━━━━━━●━━   0.800        over the visible range
Out       0.000                     read-only until tangents move
In        0.000
Locked    [x]
X step    ━●━━━━━━━━   0.010
Y step    ━●━━━━━━━━   0.010
```

Turning `Point`, `X` or `Y` is the same edit the keys make; the form is
rebuilt from the curve after, so its ranges follow the selection. `Tab`
in the form cycles `Point`: the form's cycle field is a name its owner
sets, `map` for the normal map and `point` here, a small amendment to
`form.md`. `u` in the form undoes the source buffer, as on the plot.
`Enter` prefills `:tool curve <field> <value>`.

## Tests

- `find` on the C++, Rust and Python samples returns three points with
  the right token spans; on a cursor inside an inner group it takes the
  outer list; on a cursor outside any list it returns nothing.
- `read` with the default layout maps the five numbers of a C++ point;
  with `x,y` a Python pair; with `_,x,y` skips a leading field; a short
  point has zero tangents; `1` reads as locked.
- `rewrite` keeps `f`, keeps decimals, adds the step's decimals to an
  integer, keeps a sign, and writes `true`/`false` or `1`/`0` as the token
  had it.
- `eval` at a point is its `y`; between two points with zero tangents it
  is the smooth step; with matching slopes it is the line; outside the
  range it is flat.
- `:set editor curve` on an image says `no curve under the cursor` — a
  picture has no cursor in text — and so does a text cursor outside a
  list; on the sample it opens the plot right of the source, the form at
  the edge, and focuses the plot.
- `:set editor curve` in an empty `{}` seeds the linear preset as one
  undo step and opens on it.
- `j` on the plot rewrites the selected point's `y` token in the buffer
  by a tenth and nothing else in the file changes; `5k` by five; `l` and
  `h` move `x` by a tenth and stop at the neighbours and at `0` and `1`;
  `Tab` and `Shift-Tab` cycle the selection and the status row follows;
  `Enter` prefills `:tool curve y 0.8`.
- `a` inserts a group shaped like its neighbour with the curve's value at
  the midpoint and selects it; from the last point it goes halfway back;
  `x` removes a group and the separator after it, is refused on the first
  and last points and at two points.
- `render` is the unit square at 512 a side plus margins; a point past
  the square grows the picture at the same scale.
- `u` on the plot restores the buffer's text and the plot redraws from it;
  editing a number by hand in the source redraws too.
- `:w` on the plot writes the source's file; `:q` on the plot closes plot
  and form and focuses the source; `Esc` focuses the source and leaves
  the tool open.
- `:tool curve x 0.5` moves the selected point and clamps to its
  neighbours; `:tool curve layout x,y` re-reads; `:tool curve point`
  reports `curve point=2`.
- Deleting the list by hand says `curve lost`.
- The plot's pixels change and its generation moves after any edit; the
  status row says `point 2 of 3  x 0.500 y 0.800` and `CURVE`.
