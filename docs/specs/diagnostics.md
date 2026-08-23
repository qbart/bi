# Diagnostics

The LSP core stores diagnostics per buffer, converted to char offsets and
remapped through every edit (`docs/specs/lsp.md`). This feature makes them
visible and reachable. It adds no protocol: everything here reads state that
already exists.

## Status

**Built.**

## What shows

Three marks per diagnostic, all through machinery that already exists:

- **The range wears the severity's style** — a `Decoration::Repaint` on the
  `Under` layer, like a `TODO:` tag, so a selected line still looks selected.
  The style comes from four new theme keys: `diag_error`, `diag_warning`,
  `diag_info`, `diag_hint`. The built-ins colour the text and underline it —
  a bare underline is invisible at a glance in a terminal, and a recolour
  without one reads as syntax. A theme is free to disagree.
- **A sign in the gutter cell** — the cell `gutter = 1` has held open since
  the day it was reserved "for a git sign, a diagnostic, a breakpoint";
  diagnostics are its first client. `•` in the severity's colour — and only
  the colour: the underline the range styles carry comes off, since it marks
  a span of text and this is a mark about the line — the worst
  severity on the row winning. The core exposes
  `Editor::gutter_signs(window, rows)`; painting it is the frontend's job,
  because the gutter has always been the frontend's to draw.
- **The message at the end of the cursor's line** — a `Decoration::Eol`,
  first line of the message only, with `(+n)` when the row holds more. Only
  the cursor's row: a message on every marked line is a screen shouting, and
  the cursor is where the question "what is wrong here" is being asked.

`diagnostics = true` in `[options]` gates all three — the *drawing*, never
the storing, so `:lsp` still counts them and turning the option back on costs
nothing. Per-filetype and `:set` come free, as with every option.

## Navigation

`]d` and `[d` jump to the next / previous diagnostic start, wrapping, and put
the message on the status line — which is also the fallback way to read a
message too long for its EOL tail. The keys resolve to `:dnext` / `:dprev`
exactly as `ga` resolves to `:alt`: typeable without the binding, testable
without a key, and `<leader>`-bindable through the names `diagnostic_next` /
`diagnostic_prev`.

`[` and `]` become pending prefixes in normal mode — vim's bracket family —
claimed only when no operator is pending, so `di[` still reads as the text
object it always was.

## Deliberately not here

Severity filtering, a diagnostics list pane (`Results` can hold one later),
virtual lines, and counts in the statusline.
