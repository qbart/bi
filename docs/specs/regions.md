# Regions — one answer to "what does this act on"

Every editing command in bi has to answer the same question before it can do
anything: *which characters?* Today each one answers it privately, from the
same three raw ingredients — `Selections`, `Mode::visual()`, and an optional
`LineRange` — and re-derives the answer at the point of use. There are nine
such derivations in `editor.rs` and no two of them agree.

This is the spec for making that one value, computed once, passed down.

## Status

**Proposed.** Written after `:'<,'>case` over a rectangle was found to act on
whole lines.

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

/// One span of one row. Never crosses a line terminator.
pub struct Span { pub row: usize, pub start: usize, pub end: usize }

/// What an operation applies to.
///
/// Spans are sorted, disjoint, and each lies inside a single row — so
/// charwise, linewise and blockwise are the same data, and differ only in how
/// they were built.
pub struct Region { shape: Shape, spans: Vec<Span> }
```

The row-clipping is the load-bearing decision. It is what makes
`case`, `s`, `r`, `retab`, `trim`, `~`, `u`, `U` and `surround` **all the same
operation**: for each span, back to front, replace the slice. A rectangle stops
being a special case and becomes a different list of spans.

It is also why `Region` is not `Selections`. `Selections` sorts and merges
(`selection.rs:170`) — two rows whose spans meet at a line end would silently
weld together, which is exactly why `blockwise.md` derives the rectangle
instead of storing it. `Region` has no merge invariant, because a region is
not a set of cursors; it is the answer to one question, thrown away afterwards.

### Building one

Two constructors, and they are the only places intent becomes spans:

```rust
impl Region {
    /// From what is on screen: every selection, shaped by `kind`.
    pub fn of(buffer: &Buffer, sels: &Selections, kind: Option<VisualKind>,
              to_eol: bool) -> Region;

    /// From a `:` line's addresses. Always whole rows.
    pub fn rows(buffer: &Buffer, range: LineRange) -> Result<Region, String>;
}
```

`Region::of` absorbs `block_spans`, `selection_spans`, `rows_of` and
`block_columns`. Multi-cursor needs no arm: three collapsed selections are
three one-char spans, and `:case` over them is the same loop as everything
else. `for_each_selection` stops being the multi-cursor path and becomes an
implementation detail of one method.

### Applying one

```rust
impl Region {
    /// Rewrites every span, last first, and reports the edits.
    pub fn rewrite(&self, buf: &mut Buffer, f: impl Fn(&str) -> String) -> Vec<Edit>;

    /// Cuts every span and returns the register entry, shape included.
    pub fn take(&self, buf: &mut Buffer, op: Operator) -> (Entry, Vec<Edit>);
}
```

Back-to-front ordering is written once, here, with the comment it has been
given four times already. The `Vec<Edit>` is the existing `Edit::map`
(`buffer.rs:61`), so carrying selections across an edit becomes one line at
every call site and the three fixup strategies collapse to one.

`Shape` replaces `EntryKind` outright — one enum for "this text is a
rectangle", not two that must be kept in step. `Extent` keeps its counts but
takes its shape from the same enum.

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

Each command declares what it can take, once, in the table where it is named
— instead of every command re-deciding:

```rust
enum Takes {
    Spans,   // any region: case, s, retab, trim, surround, r, ~, u, U
    Rows,    // whole lines only: m, sort, >, <, J
    Nothing, // w, q, e — a range is an error, as it already is
}
```

A `Rows` command handed a block or charwise region **widens to the rows it
touches and says so** in the status (`"whole lines"`), rather than each command
inventing an answer. One rule, written down, visible when it fires.

## Where it lives

`src/region.rs`. `Shape`, `Span`, `Region`, the two constructors and the two
appliers — no `Editor`, the same way `range.rs` never learns what a `Buffer`
is. It takes `&Buffer` because spans are char offsets into a rope and there is
no honest way around that, but it holds no editor state and its tests build a
rope and assert on spans.

`docs/specs/scopes.md` is `S` and tree-sitter nodes; the name `Region` avoids
the collision.

## Migration

Each step leaves the tree green and is a commit.

1. `src/region.rs` with `Shape`, `Span`, `Region`, `Region::of`,
   `Region::rows`, and unit tests. Nothing calls it.
2. `Region::rewrite` / `Region::take` with the edit map. Still nothing calls it.
3. **Fix the test helper first.** `ex()` becomes `EnterCommandMode` +
   `CommandChar`s + `CommandExecute`, i.e. the path a keystroke takes. The two
   blockwise `:case` tests go red, and stay red until step 5 — that is the
   point of doing this before any of the rest.
4. Rewrite the ten call sites in the table to build a `Region` and hand it on.
   `SurroundSelection` gains its missing arm for free; `substitute` and
   `retab` take a `Region` instead of `(first, last)`.
5. `'v` in `range.rs`, the prefill, the `Takes` table, and delete
   `interrupted_visual`.
6. `EntryKind` → `Shape`; `Extent` takes its shape from `Shape`.
7. `for_each_selection` becomes private to `Region`; the shift-by-delta loop
   and the two hand-written back-to-front loops are deleted.

## Tests

- **The path is the product.** Every `:` test goes through `CommandExecute`.
  A helper that calls `run_ex` directly is how this bug lived; it does not come
  back.
- `:case upper` over a rectangle takes the columns and leaves `let` alone —
  `upper`, not `lower`, so whole-line and column answers differ.
- `:'<,'>case upper` over the same rectangle takes the rows, because that is
  what the user typed.
- `:'v s/…` over a rectangle substitutes inside the columns only.
- `S(` over a rectangle wraps each row's span.
- `:'v case snake` with three cursors respells three names.
- `:'v m +1` reports `whole lines` and moves the rows.
- `Region::of` on a rectangle overhanging a short row yields an empty span for
  it, and the span stays in the list.
- A region built from `n` selections, rewritten with a function that lengthens
  the text, leaves all `n` selections on the right characters.
