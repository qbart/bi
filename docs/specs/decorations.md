# Decorations

Everything bi draws that is not buffer text was, until now, computed inside
`src/tui/render.rs`: the syntax spans, the search highlight, the selection, the
extra carets. That was fine while the list was four things the renderer could
be expected to know. It stops being fine at the next seven, which are all the
same shape and none of which are the frontend's business:

- vertical indent guides
- `TODO:` / `FIX:` / `NOTE:` picked out of comments
- `#ffaacc` drawn in the colour it names
- the letters `s`, `S` and `<Tab>` put on screen to jump by
- a flash on what was just yanked
- inline diagnostics, later, when there is an LSP to have them

Written one at a time in the renderer, each is a frontend-only hack that a
second frontend inherits none of, and the core cannot answer the question "what
is on screen here" about its own buffer. So they go through one model.

## Status

**Built**, with indent guides as the first client — see `indent.md`.

## The model

```rust
pub enum Decoration {
    /// Repaint the cells a char range already occupies. The text is unchanged.
    Repaint { range: Range<usize>, style: Style, layer: Layer },
    /// Draw `text` over the cells at (`row`, `col`), replacing what is there
    /// and moving nothing.
    Overlay { row: usize, col: usize, text: String, style: Style, layer: Layer },
    /// Draw `text` after the end of `row`.
    Eol { row: usize, text: String, style: Style },
}
```

Three variants rather than one struct with an anchor and a payload, because
every combination of the two that would be legal is one of these three and the
rest are nonsense a type should not be able to say.

**`Repaint` is in char offsets and `Overlay` is in display columns**, which
looks like an inconsistency and is the whole point. A `TODO:` is a range of
*text* and must follow it as the line is edited; a guide at column 4 of a
tab-indented line sits *inside a tab* and has no char offset at all. One
coordinate system could not express both, and the choice per variant is made
where the answer is known.

**`style` is resolved, not named.** The provider looks the colour up in the
theme and hands over a [`crate::theme::Style`]. The fallback walk and the
naming policy stay in the library, and a frontend does exactly what it already
does with a syntax span: convert one struct of colours into its own. A colour
computed from the text — `#ffaacc` is its own swatch — arrives the same way, so
there is nothing a theme key could have expressed that this cannot.

**`Layer` is `Under` or `Over`**, and it is about the selection. Guides,
swatches and comment tags belong under it: selecting a line must look like
selecting a line. Jump labels belong over it: a letter you are about to press
has to be readable wherever it lands. Two values because two is what the
clients need; a z-order integer would be a number nobody could choose.

## What is deliberately not here

**Inline virtual text** — text inserted mid-line that pushes the rest of the
line right. It is the one placement with a cost that is not local: display
column stops being a function of char column, so the cursor, the mouse, every
`display_col` call and the block-selection arithmetic all have to route through
the decoration list. `Eol` covers what inline diagnostics actually want (a
message after the code, not inside it), and the day something genuinely needs
mid-line insertion is the day to pay for it.

**Wrapping and folding** are not decorations either. Both change which rows
exist, which is a different question from what is drawn on one.

## Who produces them

```rust
impl Editor {
    /// Everything to draw over `rows` of `window`, in paint order.
    pub fn decorations(&self, window: WindowId, rows: Range<usize>) -> Vec<Decoration>;
}
```

One call per pane per frame, bounded by the rows on screen — never by the size
of the file, which is the same rule the highlight pass already follows. The
providers are ordinary functions that take what they need and push onto a
`Vec`; they are gathered here so that a frontend asks one question, and so that
the order they paint in is decided once, in the core, rather than by whichever
order a renderer happened to call them in.

Nothing is cached. A decoration is derived from the buffer, the options and the
theme, all of which the answer is recomputed from every frame anyway; a cache
would need invalidating on every edit, every `:set` and every scroll, which is
strictly more work than the derivation it would be avoiding.

## What the frontend does

For each row it is already formatting:

- `Repaint` — split the spans at the range's edges and patch the style, which
  is exactly what the search highlight and the selection already do. The
  existing `paint_range` is that function; it did not have to change.
- `Overlay` — split the spans at the column and put a span of the decoration's
  own text there, dropping as many columns as the text is wide.
- `Eol` — push a span after the last one.

`Under` decorations are painted after the syntax spans and before the search
and selection passes; `Over` ones after everything. That order lives in the
renderer because painting order *is* rendering; what lives in the core is the
`Layer` value that says which group a decoration is in.

## Tests

- `Repaint` restyles the columns a char range covers, and no others, on a line
  with tabs in it — the conversion from chars to columns is the part that can
  be wrong.
- `Overlay` replaces exactly as many columns as its text is wide and leaves the
  line the same length, so nothing after it shifts.
- An overlay inside a tab's expansion lands on the column it asked for.
- `Eol` lands after the last character and does not pad the line.
- A decoration under the selection is painted over by it; one over it is not.
- `decorations()` asks for the visible rows only, and a provider that is off
  produces nothing at all.
- A whole frame, through a test backend: guides on the screen, in the columns
  the core named, past the gutter the frontend owns. A decoration produced and
  not painted looks exactly like one never produced, and only a rendered frame
  can tell the two apart.
