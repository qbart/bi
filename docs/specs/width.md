# Display width — cells, not chars

For a long time every column in bi was a `chars().count()`. That is correct
for ASCII and quietly wrong for everything else: a CJK character occupies two
terminal cells, a combining mark occupies none, and the moment either appears
on a line the cursor drifts off its character, highlights land beside their
text, and pads and truncations miscount.

## Status

**Built.**

## The rule

One function answers "how wide is this character":

```rust
// indent.rs
pub fn char_width(ch: char) -> usize   // unicode-width's answer, 0 for None
```

`unicode-width` is the dependency — the same tables ratatui itself uses to
lay glyphs into cells, which is what makes bi's arithmetic and the terminal's
painting agree. Control characters answer `None` there and 0 here. Tabs are
deliberately not its business: a tab is elastic, and the two column walkers
that meet one (`display_col`, `expand_tabs`) handle it against the tab stops
and use `char_width` for everything else.

Two kinds of number flow through the code, and only one of them changed:

- **Display columns** — anything that positions, pads, truncates or splits
  what the screen shows. All of it now sums `char_width`: `display_col` and
  `width_of` in the core; and in the TUI the span splitters
  (`split_at_col`, `paint_range`, `overlay`, `insert_inline`), `fill_line`'s
  used-width, the status line's padding, the picker's row truncation and
  query cursor, and the completion menu's measure — via one `span_width`
  helper, since spans are tab-expanded already.
- **Char offsets** — rope indices, selection ends, clamps like
  `raw.chars().count()` that feed `display_col` an *index*. Untouched:
  they were never widths.

## Splitting between cells

A cell boundary can land inside a character now, and a splitter has to
answer for it:

- **A wide char cut in half goes right, whole.** Half a glyph is not a thing
  a terminal can draw. The left half comes up a cell short, which the pad
  that follows fills.
- **A zero-width char stays with its base.** A combining mark at the cut
  belongs to the character before it; splitting between base and mark would
  strand the mark at the start of the right half, where it would combine
  with nothing.
- **Painting covers any overlap.** `paint_range` styles a char when any of
  its cells fall in the range — a selection that starts on the second cell
  of a wide char still visibly includes it.

## What this does not do

No grapheme-cluster segmentation. The cursor still moves by `char`, which is
what the buffer is indexed by; a combining mark is width 0, so the cursor on
one sits on its base's cell and nothing misaligns. What per-char width cannot
get right is the multi-char emoji (ZWJ sequences, flags), which counts as the
sum of its scalars rather than as one glyph. That needs `unicode-segmentation`
and grapheme-aware motions, is invisible in code and rare in prose, and is
not worth the dependency until it is.

## Tests

- `display_col`: two CJK chars are four cells; a combining mark adds
  nothing; a tab after a wide char still reaches the next stop.
- The span splitters: covered by the existing render tests, which assert
  lines come out the width they went in.
