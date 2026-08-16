# Blockwise visual

`selections.md` deferred `Ctrl-V` with the observation that a block is not a
range — it is a rectangle, and it does not fit `Selection` without either a
third mode flag on every selection or one selection per line. It also called
the answer: blockwise is a way of *creating* several selections rather than a
new kind of one.

That is what this is. The rectangle itself is never stored as selections; it is
**derived** from the two corners the user has already given us, and it turns
into selections only at the moment something acts on it.

## Status

**Built**, and checked against vim through `scripts/vim_differential.py`.

Scope is vim parity: select, adjust, `d` `x` `c` `y`, `I` `A`, `r`, `$`, a
blockwise register kind so `p` puts a rectangle back, and `.`.

## The model

```rust
pub enum VisualKind {
    Char,
    Line,
    Block,   // new
}
```

The mode is the flag. The primary selection's `anchor` and `head` are opposite
corners of the rectangle, and every column span is worked out on demand:

```rust
/// One (start, end) char range per row the block covers, top to bottom.
/// Rows too short to reach the left edge come back empty, and stay in the
/// list — a block is a rectangle even where the text isn't.
fn block_spans(&self) -> Vec<(usize, usize)>
```

Left edge is `min` of the two columns, right edge is `max`, and the right edge
is **inclusive** of the head's column — the same rule charwise visual already
follows. Each row's span is clamped to that row's length.

Three things fall out of deriving rather than storing:

- **Motions need no change at all.** `j`, `}`, `fx`, `G` move the head, as they
  do in every other visual mode, and the rectangle follows because it is a
  function of the head.
- **`o` still works.** It swaps the corners diagonally. `O`, new here, swaps
  the columns and keeps the rows — vim's horizontal flip.
- **Nothing has to defend the `Selections` invariants.** Materialising a block
  eagerly would push spans through `set()`, which sorts and merges; two rows
  whose spans meet at a line end would silently weld together. Deriving keeps
  the rectangle out of that machinery until an edit, where the spans are
  separated by a newline each and merging cannot trigger.

The cost is that **block mode is single-selection**: `Ctrl-V` collapses to the
primary cursor, and `Ctrl-N` refuses with a status message while a block is up.
Vim has no multi-cursor to lose, and the operations that matter — `c`, `I`, `A`
— hand back one cursor per row, which is the direction worth having.

### Ragged right — `$`

`$` in block mode extends every row to its own line end, however long each one
is. It is a flag rather than a column, because no column can express it:

```rust
/// `$` in block mode — the right edge is each row's line end.
block_to_eol: bool,
```

Set by `Motion::LineEnd` while the mode is `Visual(Block)`, cleared by any
other motion and on entering block mode. This is vim's `curswant = MAXCOL`
with the name spelled out.

## Operators

`Action::OperateSelection` grows a block arm. It does not go through
`for_each_selection`, because the selections do not exist yet:

1. Take `block_spans()`.
2. Slice each span. The texts, joined with `\n`, are one `Entry` with the new
   `EntryKind::Blockwise` — one register entry, not one per row, because what
   was taken is a rectangle and pasting it back has to know that. Rows too
   short contribute an empty line, which is how the rectangle keeps its shape.
3. For `d` and `c`, cut the spans **bottom to top**. Every cut shifts what is
   below it and nothing above it, so descending order keeps each span's
   position valid without a correction pass. Same reason as
   `for_each_selection`'s ordering, arrived at from the other end.
4. Land the cursor on the top-left corner (`d`, `y`), or leave one collapsed
   cursor per cut row and enter insert mode (`c`).

`x` is `d` and `s` is `c`, as everywhere else.

**`c` diverges from vim, deliberately.** Vim takes the text typed on the first
line and replicates it down the block when you press `Esc`. bee already has
multi-cursor, so the cursors are real and the text appears on every row as it
is typed. The buffer ends up identical; only the feedback is better.

## `I` and `A`

```rust
Action::BlockInsert { append: bool }
```

Place a collapsed cursor per row — at the left edge for `I`, one past the right
edge for `A` (or the line end, when `$` is in force) — and enter insert mode.
Multi-cursor insert does everything after that, which is the whole reason this
feature waited for multi-cursor.

Two rules for rows the rectangle overhangs:

- **`I` skips them.** A row that does not reach the left edge gets no cursor,
  matching vim.
- **`A` pads them** with spaces out to the column, so appended text lines up.
  Vim pads on `Esc`; bee pads on entry, which is visible while typing and is
  the same edit either way.

Padding is part of the insert session's undo group, so one `u` takes the
padding and the typed text together. The divergence: escaping without typing
anything leaves the padding behind, where vim leaves nothing. Cheap to live
with, and `u` is right there.

## `r` over a selection

Today `r` in visual mode falls through to normal mode and replaces one
character. Vim replaces every selected one. This adds:

```rust
Action::ReplaceSelection(char)
```

and it covers all three visual kinds, not just block — charwise and linewise
were quietly wrong, and one action fixes all three. Line terminators are never
replaced. Implemented over `Buffer::replace_chars` per row, which already
exists and already refuses to run off a line end.

## Registers and paste

```rust
pub enum EntryKind {
    Charwise,
    Linewise,
    Blockwise,   // new
}
```

The entry's text is the rows joined with `\n`, with no trailing newline — the
newlines are separators inside the rectangle, not terminators of lines.

`Buffer::paste` grows a third arm. For row *i* of the entry, starting at the
cursor's row and column (`P` at the column, `p` one past it, matching charwise):

- Pad the target row with spaces if it is shorter than the column.
- Insert row *i*'s text, repeated `count` times — `3p` of a block widens each
  row three times, as vim does.
- Append a new row when the buffer runs out of them.

Each row's insert point is recomputed from `line_to_char` as it goes, so the
pass runs top to bottom without a shift correction. The cursor lands on the
top-left corner of what was put in.

## `.`

```rust
enum Extent {
    Chars(usize),
    Lines(usize),
    Block { rows: usize, cols: usize },   // new
}
```

`selection_extent` reports the rectangle's size, and `repeat_over` re-cuts a
block of that size from the cursor before applying the operator — the same
trick already used for charwise and linewise, with two dimensions instead of
one.

## Rendering

The selection pass in `render.rs` gets a `Block` arm: for each row inside the
row range, paint columns `left..=right`, or `left..line_end` when `$` is in
force. It reads the corners rather than `selection.range()`, since a char range
says nothing about columns. Everything else there — the search pass under it,
the extra-cursor pass over it, tab expansion through `display_col` — is
untouched.

The status label is `V-BLOCK`.

## Keys

| Key | Does |
|---|---|
| `Ctrl-V` | start blockwise, or leave it if it is already up |
| motions | move the head; the rectangle follows |
| `o` | swap corners diagonally |
| `O` | swap columns, keep the rows |
| `$` | ragged right edge — every row to its own end |
| `d` `x` | cut the block |
| `c` `s` | cut it and put a cursor on every row |
| `y` | yank it as a rectangle |
| `I` `A` | insert at the left / right edge of every row |
| `r{char}` | overwrite every character in the block |
| `p` `P` | put a yanked rectangle back |

`Ctrl-Q` is not an alias. Vim has one because `Ctrl-V` is paste on Windows
terminals; if that ever matters here it is one line in the keymap.

## Testing

Unit tests in `editor.rs` and `buffer.rs` in the existing style, and cases in
`scripts/vim_differential.py` — `Ctrl-V` is `\x16` in a key string, so the
whole feature is testable against the oracle it is copying. Parity was the ask,
and vim is the thing that says whether it was met.

The cases worth having there: `d` and `y`+`p` over a plain block, a block that
overhangs short lines, `$A`, `I` where a row is too short, `r`, and `.` after a
block delete.

## Deferred

**`gv`** still applies here — reselecting a block needs the corners kept, not a
range, which is one more reason it wants doing once rather than per kind.

**Blockwise `~`, `u`, `U`** — case operators over a rectangle. Nothing about
the design blocks them; they are just not the reason anyone reaches for
`Ctrl-V`.
