# Horizontal scrolling

Long lines used to clip at the pane's edge and the cursor kept walking —
off the screen, past it, position reported to the terminal anyway. Now the
window slides sideways after the cursor, the way it has always slid down.

## Status

**Built.** No wrap mode: bi does not wrap lines, and this is the other
answer to a long line. No explicit scroll keys either — `zh`/`zl` can arrive
later if anyone misses them; the window following the cursor covers the use.

## The model

One new number, the horizontal mirror of the one that was there:

```rust
// window.rs
pub struct Text {
    pub scroll: usize,   // first visible row
    pub left: usize,     // first visible display column of the text area
}
```

`left` is **display columns**, not chars — tabs and wide chars make the two
disagree, and the screen is ruled in columns (see [width.md](width.md)). It
lives on `Text` because it is per-view state: two windows on one buffer
scroll sideways independently, exactly as they scroll down.

## Following the cursor

`View::scroll_to_cursor` — called once per window per frame from
`size_window`, with the width the frontend last reported — clamps `left`
after it clamps `scroll`, with the same `SCROLLOFF` margin capped to half
the pane:

- cursor column left of `left + margin` → `left` falls to keep the margin;
- cursor column past `left + width - margin` → `left` rises the same way;
- a width of zero — a test, a headless embedder that never reported one —
  means no viewport and no clamping, rather than pinning `left` to the
  cursor.

The width that matters is the text's: the window's width minus the gutter,
by `Options::gutter_width` — already a core fact — and zero gutter under
`:zen`. The cursor's column is `display_col` over the line, so a tab
scrolls by what it occupies.

Vertical-only commands (`Ctrl-E`/`Y`/`D`/`U`) stay vertical-only; the
horizontal clamp runs on the next frame and follows wherever the cursor
ended up.

## Drawing it

The renderer builds each row exactly as before — syntax, decorations,
search, selections, inline labels, all speaking absolute display columns —
and slides it at the very end: keep the first `gutter` columns, drop the
next `left`, show the rest. One splice per row, and nothing upstream ever
learns the window slid sideways. The gutter never scrolls; text does.

The cursor's screen cell subtracts `left` and clamps to the pane. Floats
(hover, completion, signature) anchor through `anchor_cell`, which now takes
`left`: an anchor scrolled off to the left is off screen the same as one
scrolled off above, and the float stays home.

Splicing at a cell boundary inside a wide char follows width.md's rules:
the straddling char goes to the visible side whole, a combining mark stays
with its base.

## Tests

- The cursor walks off the right edge and `left` follows, margin included;
  walking back to column zero brings `left` home.
- Fifty CJK chars scroll as a hundred cells — the offset counts what the
  screen shows.
- No reported width, no sideways scrolling.
