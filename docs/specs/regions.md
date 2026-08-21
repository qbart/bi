# Regions — one answer to "what does this act on"

Every editing command in bi has to answer the same question before it can do
anything: *which characters?* Today each one answers it privately, from the
same three raw ingredients — `Selections`, `Mode::visual()`, and an optional
`LineRange` — and re-derives the answer at the point of use. There are nine
such derivations in `editor.rs` and no two of them agree.

This is the spec for making that one value, computed once, passed down.

## Status

**Built.** Written after `:'<,'>case` over a rectangle was found to act on
whole lines, and then built out to the shape below.

## The bug that started it

```
Ctrl-V, select columns 4..8 of three rows
:case upper          (the `:` line prefills `'<,'>`)
```

| | |
|---|---|
| expected | `let ALPHA = 1;` — the columns |
| actual | `LET ALPHA = 1;` — the rows, and the status says `3 lines recased` |

`recase` has a blockwise arm (`src/editor.rs:6404`) that does the right thing.
It never runs. Two lines apart, in a file of 14,600:

```rust
// editor.rs:3974 — Action::CommandExecute
let Mode::Command(line) = std::mem::take(&mut self.session.mode) else { … };
self.run_ex(&line);                     // mode is now Mode::Normal
```

```rust
// editor.rs:741 — Session::visual()
Mode::Command(_) => self.interrupted_visual,   // ← only reachable in Command
_ => None,
```

`mem::take` resets the mode to `Normal` *before* the command runs, so the one
accessor that knows about the rectangle answers `None` to the one caller that
asks. `interrupted_visual` still holds `Some(Block)`; nothing can see it.

### Why the tests say otherwise

`case_over_a_rectangle_takes_the_columns_and_not_the_lines` passes. It calls
the test helper

```rust
fn ex(ed: &mut Editor, line: &str) { ed.run_ex(line); }     // editor.rs:7501
```

which enters through the back door: `Action::CommandExecute` never runs, the
mode is still `Mode::Command`, and the blockwise arm fires. **The test exercises
a path no keystroke can reach.** Both blockwise `:case` tests are false
positives for this reason. (The first is doubly false: it uses `case lower` on
`let ALPHA = 1;`, where the whole-line answer and the column answer are the same
string.)

That is the shape of the problem, not just this instance. The kind lives in a
mode enum, the mode is control-flow state, and control flow destroys it on the
way to the code that needs it.

## The real fault

Fixing `mem::take` fixes `:case`. It fixes nothing else, because the same
question is asked and answered independently in nine places:

| `editor.rs` | asks | knows about |
|---|---|---|
| `selection_extent` :5041 | `mode.visual()` | chars / lines / block |
| `selection_spans` :5081 | `mode.visual()` | block, then folds the rest |
| `rows_of` :5090 | `mode.visual()` | lines, else charwise |
| `paste_over_selection` :5197 | `mode.visual()` | all three |
| `ReplaceSelection` :5499 | `mode.visual()` | via `selection_spans` |
| `OperateSelection` :5789 | `mode.visual()` | all three |
| `SurroundSelection` :5844 | `mode.visual()` | **lines only — no block arm** |
| `recase` :6404 | `session.visual()` | all three, unreachably |
| `substitute` :6499 | *nothing* | rows only |
| `retab` :6707 | *nothing* | rows only |

Three further symptoms fall straight out of the table:

- **`S(` over a rectangle** wraps the whole character range between the
  corners, brackets and all, instead of each column span.
- **`:'<,'>s/…` over a rectangle** substitutes whole lines.
- **`:'<,'>case` over a *charwise* selection** takes whole lines too. That one
  is written down as intended in `ranges.md`, but it is the same hole: the
  command was handed rows because rows were all the caller could express.

And there are three parallel taxonomies for one concept:

```rust
enum VisualKind { Char, Line, Block }    // what is on screen
enum EntryKind  { Charwise, Linewise, Blockwise }  // what is in a register
enum Extent     { Chars(n), Lines(n), Block { rows, cols } }  // what `.` repeats
```

plus three separate strategies for keeping positions valid across an edit:
`for_each_selection`'s shift-by-delta (:5396), `operate_block`'s and `recase`'s
hand-written back-to-front loops, and `retab`'s / `trim_for_write`'s
`Edit::map` fold.

## The model

One type. Computed at the boundary, passed everywhere, never re-derived.

```rust
// src/region.rs

/// How a region was meant. Survives so that yank, paste and `.` can ask.
pub enum Shape { Chars, Lines, Block }

/// One contiguous stretch of the buffer. Runs through line terminators.
pub struct Part { pub start: usize, pub end: usize }

/// One row's worth of a region. Never crosses a line terminator.
pub struct Span { pub row: usize, pub start: usize, pub end: usize }

/// What an operation applies to.
pub struct Region { shape: Shape, parts: Vec<Part> }
```

**Two views, because operations genuinely ask two questions.**

- `parts()` is contiguous stretches, one per selection — what an operator that
  takes text *away* works on. `d` over two selected lines takes one range with
  the newline in the middle of it, and a row-clipped list could not say that.
- `spans(buffer)` is those parts cut at every row boundary and clipped to each
  row's content — what a rewrite that must not touch a line terminator works
  on. `r`, `:case` and `:s` are all this.

A rectangle's parts are already one per row, so for a block the two views are
the same list. That is the whole of what "blockwise" means here, and it is why
`case`, `s`, `r` and `surround` stop having a blockwise arm: they ask for the
view they need and a rectangle is already in it.

`Region` is not `Selections`, which sorts *and merges* (`selection.rs:170`):
two rows whose spans meet at a line end would silently weld together, which is
exactly why `blockwise.md` derives the rectangle instead of storing it. A
region has no merge invariant, because it is not a set of cursors — it is the
answer to one question, thrown away afterwards.

### Building one

Three constructors, and they are the only places intent becomes characters:

```rust
impl Region {
    /// From what is on screen: every selection, shaped by `shape`.
    pub fn of(buffer: &Buffer, sels: &Selections, shape: Shape, to_eol: bool) -> Region;

    /// Whole rows — what a `:` line's addresses name.
    pub fn of_rows(buffer: &Buffer, first: usize, last: usize) -> Region;

    /// From explicit char ranges — what a text object gives.
    pub fn spanning(shape: Shape, ranges: impl IntoIterator<Item = (usize, usize)>) -> Region;
}
```

`Region::of` absorbs `spans_of_block`, `block_columns_of`, `selection_spans`
and `rows_of`, which are all gone. Multi-cursor needs no arm: three selections
are three parts, and every operation over them is the same loop as everything
else.

`Region::part_of` is the same question for one selection at a time, for the
operators that walk their selections capturing a register entry each. It is
what keeps `line_range` from being spelled out again at every such loop.

### Applying one

```rust
impl Region {
    /// Rewrites every part, last first, and reports the edits.
    pub fn rewrite(&self, buf: &mut Buffer, f: impl Fn(&str) -> String) -> Vec<Edit>;
    /// The same, a row at a time.
    pub fn rewrite_rows(&self, buf: &mut Buffer, f: impl Fn(&str) -> String) -> Vec<Edit>;
    /// Applies an operator over every part.
    pub fn cut(&self, buf: &mut Buffer, op: Operator);
    /// The whole region as one string, spelled the way its shape pastes back.
    pub fn text(&self, buf: &Buffer) -> String;
}
```

Back-to-front ordering is written once, here, with the comment it had been
given four times. The `Vec<Edit>` is the existing `Edit::map` (`buffer.rs:61`),
so carrying selections across an edit is one line at every call site.

`Region::text` is the one place a shape decides how text is spelled — a
rectangle joins its rows with `\n` and stops, lines always end in one — so a
register entry and the region it came from cannot disagree.

`Shape` replaces `EntryKind` and `VisualKind` outright: one enum for "this text
is a rectangle", not three that must be kept in step.

## Where the intent is said

The bug above is a *transport* failure: the shape was known when `:` was
pressed and gone when Enter was. `interrupted_visual` was the patch for that,
and it is a hidden field that one accessor reads under one mode — the least
visible place the shape could live.

The fix is to stop hiding it. **The `:` line already shows you what it will act
on; make it show the truth.** The range language grows one address:

```
'v      the selection itself — its exact characters, whatever shape it has
'<,'>   the rows the selection touches            (vim's meaning, kept)
```

- **Charwise** `'v` is the selected characters.
- **Linewise** `'v` is the selected rows — identical to `'<,'>`, and that is
  correct, not redundant.
- **Blockwise** `'v` is the rectangle's columns.
- **Multi-cursor** `'v` is every selection.

`Action::EnterCommandMode` prefills `'v` when a selection is up. Typing
`'<,'>` over it is how you say *no, the whole lines* — the thing you cannot
say today. The status line's `V-BLOCK` stays up while you type, because
rendering already reads through `Session::visual()`.

This is the whole of the fix to "it should show that it operates on the
selection": the scope is **named in text you can see and edit**, so it cannot
be lost by a mode transition, cannot be quietly widened, and needs no accessor
that is only correct in one mode. `Session::interrupted_visual` is deleted;
the resolved `Region` is snapshotted into `Mode::Command` when the line opens.

### What a command does with one

A command that can only work in whole lines — `:m` cannot move half a row,
`:retab` cannot indent half a line — calls `View::whole_rows`, which **widens
to the rows the region touches and says so** in the status (`whole lines`).
One rule, in one place, visible when it fires, rather than each command
inventing an answer.

A command with no scope written names its own default, and the four defaults
that exist are one enum where the commands are dispatched:

```rust
enum Fallback {
    Words,          // the word under each cursor — `:case`
    CursorRow,      // `:s`
    File,           // `:retab`
    SelectionRows,  // `:m`
}
```

A range handed to a command that takes none is still an error, as it was.

## Where it lives

`src/region.rs`. `Shape`, `Part`, `Span`, `Region`, the constructors and the
appliers — no `Editor`, the same way `range.rs` never learns what a `Buffer`
is. It takes `&Buffer` because a span is a char offset into a rope and there is
no honest way around that, but it holds no editor state, and its tests build a
rope and assert on text.

`docs/specs/scopes.md` is `S` and tree-sitter nodes; the name `Region` avoids
the collision.

## What changed, in order

1. **`Shape`.** `VisualKind` and `EntryKind` become one enum, in `region.rs`.
2. **`Region`,** with both views and the appliers.
3. **`Scope` in `range.rs`,** the `'v` spelling, and the prefill.
4. **The `:` commands** — `:case`, `:s`, `:retab`, `:m` — resolve a scope
   through `View::region` and work on what they get. The shape rides in as an
   argument to `run_ex_over`, taken out of the session together with the `:`
   line, so it can neither go stale nor be missing.
5. **The visual operators** — `d` `y` `c` `r` `p` `S` — ask `View::selected`.
   `SurroundSelection` gained the blockwise arm it never had.
6. **The duplicates deleted**: `spans_of_block`, `block_columns_of`,
   `span_of_block_at`, `selection_spans`, `rows_of`, `cut_spans`,
   `replace_spans`.

`for_each_selection` stays. It is not scope machinery — it is what keeps a set
of selections valid across an edit, which is a different job and the only one
it does now.

## Tests

- **The path is the product.** The `ex()` helper now goes through
  `EnterCommandMode`, `CommandChar` and `CommandExecute` — the path a keystroke
  takes. A helper that called `run_ex` directly is how this bug lived through
  two passing tests; it does not come back.
- `:'v case upper` over a rectangle takes the columns and leaves `let` alone.
  `upper`, not `lower`, so the whole-line and column answers differ — the old
  test could not have failed.
- `:'<,'>case upper` over the same rectangle takes the rows, because that is
  what was typed.
- **The prefill alone does it**: `:` then `case upper` over a rectangle, with
  nothing typed in front of the command.
- `:'v s/a/X/g` over a rectangle substitutes inside the columns only.
- `S"` over a rectangle wraps each row's span, and one `u` takes it all back.
- `:'v case camel` over three cursors with a row between them respells the
  three and not the row.
- `:'v m 0` reports `whole lines`.
- `Region::of` on a rectangle overhanging a short row yields an empty span for
  it, and the span stays in the list.
- A charwise part runs through a line end and its spans do not — the two views,
  and the one test that says why there are two.
- A rewrite that lengthens the text still lands every part, and a position is
  carried across it.
