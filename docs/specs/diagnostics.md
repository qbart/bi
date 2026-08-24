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
  diagnostics were its first client, git signs its second
  (`docs/specs/git-signs.md`), and the diagnostic wins the cell. `•` in the severity's colour — and only
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

## The list — `:diags`

The pane `Results` was built to hold one day, holding one. `:diags` collects
the stored diagnostics of every open buffer — most-severe first within a file,
buffer order across them — into the same pane `:find` and `:references` fill:
Enter jumps to the diagnosed span, `Ctrl-^` brings the file back, `:results`
reopens the list later. Each row is the offending line with the diagnosed span
highlighted, and the first line of the message appended after `▸` — the line
says where, the tail says what, and jumping is what the pane is for.

Open buffers only, because that is what the store holds: bi's diagnostics
arrive by `textDocument/publishDiagnostics`, which speaks about open
documents. A project-wide list would mean pulling `workspace/diagnostic` —
protocol the core does not speak, for a list that would mostly repeat what
`cargo build` already prints. Not out of the box, so not here.

## Deliberately not here

Severity filtering, workspace-pull diagnostics, virtual lines, and counts in
the statusline.
