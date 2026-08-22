# Floats, and hover on one

The first thing bi draws that floats: a box anchored to a buffer position,
inside the pane, over the text. Hover (`K`) is its first client and this spec
carries the surface; completion is the second and the harder one — see
`docs/specs/complete.md`.

## Status

**Built.**

## The surface

There is deliberately **no generic Popup type**. Hover and completion share
almost nothing semantically — one is passive text, the other an interactive
menu — and a core abstraction over both would be a type with two unrelated
halves. What they do share is *geometry*, and geometry has always been the
frontend's. So the shape is the picker's, twice over:

- The core holds state that does not draw: `Session.hover` here,
  `Session.completion` for the menu. Testable without a terminal.
- The frontend owns one anchored-box convention in `tui/render.rs`: measure
  the content, place it beside the anchor's screen cell, flip to the other
  side when the room runs out, clamp to the pane. Hover prefers **above**
  the anchor (it annotates what you just read); the menu prefers **below**
  (it grows under what you are typing).

One new theme key, `popup` — the float's base foreground and background.
Selection and badges inside the menu reuse `picker_selected` and
`picker_badge`, so choosing looks the same everywhere bi offers a choice.

## Hover — `K`

`K` rides the `ga` rail: it resolves to `:hover`, rebindable by the name
`hover`. The request carries the cursor's position and the anchor travels
with the intent, because by the time the answer lands the cursor may have
moved — the float belongs to the spot that was asked about.

The wire allows four shapes for the contents — a string, a
`{language, value}` pair, a `{kind, value}` markup object, or an array of
the first two — and the registry normalises all of them to one markdown
string before the editor ever sees it.

**Markdown gets minimal, honest processing**, not a renderer:

- Fence lines (```` ``` ````) are dropped; the lines between them become
  `Code` lines, highlighted through `Syntax::for_filetype` with the buffer's
  own filetype — a hover's code is nearly always the language you are in,
  and bi already knows how to colour that.
- A `---` line becomes a themed rule, drawn with the `rule` style.
- Everything else passes through as text, untouched. Stripping emphasis
  markers loses information; rendering them is a project.

The float is capped at the room the pane has; no scrolling in v1. Any next
command dismisses it — the same rule, and the same line of `apply`, as the
yank flash: what clears it is *doing anything else*.

Refusals are statuses: no server, no `hoverProvider`, or a null answer
(`no hover info here`).

## Deliberately not here

Scrolling long hovers, per-fence language highlighting (the fence's own tag
overriding the buffer's filetype), markdown emphasis rendering, and hover
ranges (underlining what the answer covers).
