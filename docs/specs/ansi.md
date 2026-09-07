# Escape sequences in text

A `:!cargo build` whose output carries colours must not paint the editor.
Two rules, at two layers: the shell strips what a job prints, and the screen
never executes a control character it was handed as text.

## Status

**Built.** Asked for by the user on 2026-09-08 after a coloured build log
left glyphs behind in the neighbouring pane.

## The bug

The buffer view writes every character that is not a tab straight into its
screen cell (`styled_line`, `src/tui/render.rs`). An `ESC [ 3 2 m` in a line
therefore reaches the terminal as bytes, the terminal *executes* it, and the
diff-based redraw goes on believing the cell holds what it wrote. Closing the
pane repaints only the cells the diff thinks changed, so the terminal's real
state and the diff disagree and stale glyphs stay behind. The results pane
met the same bug once and got `terminal_safe`; the transient buffer just
made it a one-command reproduction. Any file with an escape byte in it does
the same today, and so does the debugger's Console.

## Rule 1 — a job's output is text, not a terminal

`:!cmd` has no pty: the job's stdout is a pipe, and a well-behaved program
prints no colours into a pipe. The ones that do (`CARGO_TERM_COLOR=always`,
`--color=always`, a tool that never checks) are printing for a terminal that
is not there, and bi reads their output the way a terminal would have
*shown* it, minus the colours:

- **Escape sequences are removed:** CSI (`ESC [` … final byte `0x40-0x7E`),
  OSC (`ESC ]` … `BEL` or `ESC \`), and every other two-byte `ESC x`
  sequence; a lone trailing `ESC` goes too.
- **A carriage return rewinds the line:** the text after the *last* `\r` is
  what is kept, which is what a terminal ends up displaying for a progress
  bar that redraws itself.
- **Other C0 controls are dropped** (`BEL`, `BS`, `VT`, `FF`, …). `\t`
  stays — it is text.

This is one pure function, `shell::sanitize(&str) -> String`, in the core,
with no knowledge of buffers or screens. `pump_shell` applies it to every
`Line::Out` and `Line::Err` a job produces, and `:w !cmd` applies it to the
output it shows in the transient buffer, because that output is a log too.
**Filters (`:{range}!cmd`) and `:r !cmd` are not touched:** their output is
text you asked to have *in your buffer*, vim inserts it verbatim, and rule 2
keeps it from harming the screen.

Colours are lost, not translated. Turning SGR into highlight spans would
want a per-line colour table that survives edits, undo and the cap's trim —
a feature to add on top of this, not a fix to fold into it.

## Rule 2 — the screen never executes text

Every control character that reaches a screen cell is drawn the way vim
draws it: `^[` for `ESC`, `^A` … `^Z` for `0x01-0x1A`, `^?` for `DEL`, two
cells wide, in the span's own style. Nothing in a buffer can move the
terminal's cursor, change its colours, or erase a line.

Width and glyph come from one place so the cursor and the text agree:
`indent::char_width` answers `2` for a control character (it answered `0`,
which is why a cursor over one already sat in the wrong column), and a
sibling `indent::glyph(ch) -> Option<[u8; 2]>`-shaped helper names the two
characters to draw. `display_col` and `width_of` follow from `char_width`
without a change of their own.

Four drawing paths use it: the buffer view's `styled_line` for a highlighted
line and `render_window`'s plain branch for one with no grammar attached, the
debugger's Console lines, and the results pane — whose `terminal_safe` today
*drops* control characters, and after this shows them, the same as everywhere
else. The Console's own text is otherwise untouched; only the cells it paints
change.

## Where it lives

```
src/shell.rs         sanitize()                                        (core)
src/indent.rs        char_width() = 2 for controls; the glyph helper   (core)
src/editor.rs        pump_shell / run_write call sanitize              (core)
src/tui/render.rs    cells(): styled_line, the plain branch, console, results (tui)
```

A frontend that is not a terminal draws `^[` too, or draws what it likes:
the glyph helper is advice from the core, not a screen contract.

## Tests

- `sanitize`: SGR colours vanish and the text stays; a cursor-move CSI and an
  OSC title go; `a\rb\rc` is `c`; `BEL` is dropped; `\t` survives; a lone
  trailing `ESC` is dropped; a line with nothing to strip is returned
  unchanged.
- A job whose fake spawner pushes `\x1b[32mok\x1b[0m` shows `ok` in the
  transient buffer; a stderr line the same way, still prefixed `! `.
- `:r !` with the same fake output inserts the escapes verbatim.
- `char_width('\x1b') == 2`; `display_col` on `\x1b[1mx` puts `x` at
  column 5 (`^[` `[` `1` `m`).
- `styled_line` on a line holding `ESC` yields `^[` in the cell text and no
  raw control byte in any span; the Console and results paths likewise.
- Render a line of text with `ESC` in it, then remove it: the diff repaints
  the cells (no stale glyph) — asserted on the rendered frame.

## Deviations from the design

Five places where the build corrected or completed what this spec's text
said, each recorded here so nobody re-derives it from scratch:

1. **An `ESC` followed by an intermediate byte is consumed through to its
   final byte, not just the next one.** The spec's "every other two-byte
   `ESC x`" undercounted a whole class: `ESC` followed by a byte in
   `0x20-0x2F` — the charset designation `ESC ( B` and its kin — is a
   sequence that only ends at the first byte outside that range, `0x30-0x7E`,
   which can be several bytes further on, not the very next one.
2. **The buffer view has a fourth cell path.** `render_window` draws an
   unhighlighted line — no grammar attached — through its own branch beside
   `styled_line`, and that branch needs the glyph helper too; the spec's
   count of drawing paths left it out.
3. **The Console draws its lines against a tab width of 8.** It is not a
   buffer and has no `tab_width` of its own to consult, so its lines use the
   one width nothing else in the editor has to ask for.
4. **`terminal_safe` was renamed `cells` and generalised to every path.** The
   results pane's helper used to drop control characters for one view only;
   the build turned it into the shared function all four paths call, and the
   old, narrower name went with the old, narrower job.
5. **`display_col` on `\x1b[1mx` puts `x` at column 5, not 6.** `^[` costs 2
   cells, and `[`, `1`, `m` cost one each — 2+1+1+1 — the spec's Tests
   section had the arithmetic one column high.
