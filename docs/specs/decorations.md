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
    /// Draw `text` *between* the cells at (`row`, `col`), pushing the rest of
    /// the row right.
    Inline { row: usize, col: usize, text: String, style: Style },
    /// Draw `text` after the end of `row`.
    Eol { row: usize, text: String, style: Style },
}
```

Four variants rather than one struct with an anchor and a payload, because
every combination of the two that would be legal is one of these four and the
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

`Inline` and `Eol` have no `Layer`, and it is not an omission: neither of them
is *on* the text. One is out past the end of the line and the other makes its
own cells, so there is nothing for either to be over or under.

## `Inline`, and what it cost

An earlier draft of this file listed inline virtual text under "deliberately
not here", on the grounds that display column stops being a function of char
column and everything that computes one has to route through the decoration
list. The day came: a jump label drawn *over* the text hides the character it
is pointing at, which is the one character you were looking at, and no colour
fixes that. So it is here, and this is the bill.

**It is the last pass.** Every other column — the search highlight, the
selection, the block arithmetic, the guides — is worked out and painted before
a single label is inserted, so all of them go on seeing the row as the text
says it is. Only the terminal's own cursor has to be adjusted, by the width of
the labels at or before its column, and that is one function.

**Two at one column are two cells.** They are applied left to right, sorted by
column, with a running shift; the sort is stable, so where two labels want the
same place the order they were produced in is the order they read in. That is
what lets `S` mark two scopes that end together as `ab` instead of dropping
one — see `scopes.md`.

**Nothing that has to survive an edit may use it.** The row is wider than its
text while the decoration is up, so this is for things that are up for one
keystroke. Diagnostics still want `Eol`: a message after the code, not inside
it.

## What is deliberately not here

**Wrapping and folding** are not decorations. Both change which rows exist,
which is a different question from what is drawn on one.

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
- `Inline` — split the spans at the column and put a span of the decoration's
  own text there, dropping nothing. Sorted by column and applied with a running
  shift, so every column named is a column of the row as it stands.
- `Eol` — push a span after the last one.

`Under` decorations are painted after the syntax spans and before the search
and selection passes; `Over` ones after everything; `Inline` after those, last
of all, because it is the only one that moves a cell. That order lives in the
renderer because painting order *is* rendering; what lives in the core is the
`Layer` value that says which group a decoration is in.

## Tests

- `Repaint` restyles the columns a char range covers, and no others, on a line
  with tabs in it — the conversion from chars to columns is the part that can
  be wrong.
- `Overlay` replaces exactly as many columns as its text is wide and leaves the
  line the same length, so nothing after it shifts.
- An overlay inside a tab's expansion lands on the column it asked for.
- `Inline` drops nothing: the character it was inserted in front of is still
  there, one cell further along.
- Two `Inline`s at one column are two cells, in the order they were produced,
  and a third at a later column still lands where the original text put it.
- `Eol` lands after the last character and does not pad the line.
- A decoration under the selection is painted over by it; one over it is not.
- `decorations()` asks for the visible rows only, and a provider that is off
  produces nothing at all.
- A whole frame, through a test backend: guides on the screen, in the columns
  the core named, past the gutter the frontend owns. A decoration produced and
  not painted looks exactly like one never produced, and only a rendered frame
  can tell the two apart.
