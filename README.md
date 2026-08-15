# bee

A batteries-included modal editor. Tree-sitter, git, and LSP are meant to be
built in, not plugins.

Status: modal editing, undo, registers, and tree-sitter highlighting for Rust.
See [RECOMMENDATION.md](RECOMMENDATION.md) for why the stack is what it is, and
[docs/specs](docs/specs) for the designs behind each piece.

```sh
cargo run -- <file>
cargo test
cargo fmt --check
```

`rustfmt.toml` sets `use_small_heuristics = "Max"`, which keeps short struct
literals, match arms and `if`/`else` on one line. That is the style the code was
already written in; the default heuristics would expand a large part of it.

## Key bindings

Counts work where vim's do: `5j`, `3dd`, `d3w`, and `2d3w` multiplies to six
words. `{n}` below means an optional count.

### Normal mode

**Motions** — usable on their own, or as the target of an operator.

| Key | Moves to |
|---|---|
| `h` `l` | left, right (`Space` also moves right) |
| `j` `k` | down, up, keeping a goal column through short lines |
| `w` `b` | start of the next / previous word |
| `0` `^` | start of the line (`^` is an alias for `0` until a first-non-blank motion exists) |
| `$` | end of the line |
| `gg` | first line |
| `G` | last line, or line `{n}` when counted |
| arrows, `Home`, `End` | same as the above |

**Operators** — take a motion, or double the key for whole lines.

| Key | Does |
|---|---|
| `d{motion}` | delete over the motion: `dw` `d$` `d0` `db` `dj` `dgg` `dG` |
| `c{motion}` | change — delete, then enter insert mode |
| `y{motion}` | yank |
| `dd` `cc` `yy` | the whole line, `{n}` of them when counted |
| `Y` | `yy` |
| `x` | delete the char under the cursor — exactly `dl` |

`cw` follows vim and behaves like `ce`: it changes the word without swallowing
the whitespace after it. `dw` at the end of a line stops there rather than
pulling the next line up.

**Registers and paste**

| Key | Does |
|---|---|
| `p` `P` | paste after / before the cursor, or below / above the line if the entry was taken linewise |
| `"p` `"P` | open the picker to choose from everything captured, then paste |
| `"_` | black hole prefix — `"_dd` deletes without capturing |

Every `y`, `d`, `c` and `x` captures automatically into a 4096-deep ring, so
there is nothing to decide at yank time. A count goes before the quote: `3"p`.

**Entering insert mode**

| Key | Puts the cursor |
|---|---|
| `i` `a` | before / after the cursor |
| `I` `A` | at the start / end of the line |
| `o` `O` | on a new line below / above |

**Undo**

| Key | Does |
|---|---|
| `u` | undo, `{n}` steps when counted |
| `Ctrl-R` | redo |

One command is one undo step, so `5x` returns in a single `u`. A whole insert
session is one step, including the newline `o` opened before it. Undo is a
tree, so undoing and then typing keeps the old branch rather than discarding it
— though nothing reaches those branches yet.

**Other**

| Key | Does |
|---|---|
| `Esc`, `Ctrl-C` | back to normal mode, or cancel a half-typed command |
| `:` | ex command line |

### Insert mode

Printable keys insert themselves. `Enter`, `Backspace` and `Tab` do the obvious
thing, arrows and `Home`/`End` move, and `Esc` or `Ctrl-C` returns to normal
mode with the cursor pulled back onto a character.

### Picker

Opened with `"p` / `"P`. Typing filters by substring — every whitespace-separated
term must appear somewhere, in any order, case-insensitively. Matches stay in
ring order, so the most recent is first.

| Key | Does |
|---|---|
| any printable | add to the query |
| `Backspace` | delete a char; on an empty query, cancel |
| `Ctrl-N`, `Down` | next match |
| `Ctrl-P`, `Up` | previous match |
| `Ctrl-A` | show or hide one-character entries, hidden by default |
| `Enter` | paste the highlighted entry |
| `Esc`, `Ctrl-C` | cancel |

A `¶` beside a row means the entry is linewise and will open a new line.
Choosing an entry also moves it to the front of the ring, so a plain `p`
afterwards repeats it.

### Ex commands

| Command | Does |
|---|---|
| `:w` `:w <path>` | write |
| `:q` `:q!` | quit, refusing if there are unsaved changes unless forced |
| `:wq` `:x` | write and quit |
| `:{n}` | go to line *n* |

## Layout

bee is a library plus a frontend. The library is the editor and knows nothing
about terminals; `src/tui/` is the terminal frontend, and a GUI would be its
sibling rather than a rewrite. See [docs/specs/lib-split.md](docs/specs/lib-split.md).

**The library** — `src/lib.rs`:

| File | Holds |
|---|---|
| `buffer.rs` | rope, cursor, motions, the single mutation primitive |
| `history.rs` | the undo tree: revisions, branching, invertible `Change`s |
| `registers.rs` | the yank ring: entries, capture, eviction |
| `editor.rs` | modes, the `Action` dispatch table, ex commands, scrolling |
| `motion.rs` | `Motion` / `Operator` / `Kind` — the vocabulary they all share |
| `picker.rs` | the overlay's state: query, matches, selection |
| `syntax.rs` | tree-sitter: incremental reparse, highlight spans |
| `input.rs` | keys → `Command`; the `[count] op [count] motion` state machine |
| `key.rs` | `Key` / `KeyCode` / `Mods` — bee's own key vocabulary |

**The terminal frontend** — `src/main.rs`:

| File | Holds |
|---|---|
| `main.rs` | terminal lifecycle, event loop |
| `tui/render.rs` | viewport-bounded render pass |
| `tui/keys.rs` | crossterm key events → `bee::key::Key` |

## The seven decisions this step locks in

1. **Cursor is a char index.** Byte and UTF-16 conversions happen at the edges
   (`Buffer::point_at`), never inside motion code. LSP wants UTF-16 columns,
   tree-sitter wants byte columns; neither leaks inward.
2. **One mutation primitive.** Every edit goes through `Buffer::apply_edit`,
   which records an `Edit` carrying old/new byte ranges and points — exactly
   `tree_sitter::InputEdit`. The rope field is private to enforce this.
   `Editor::sync_syntax` drains `pending_edits` each frame — tree-sitter reads
   it today, and LSP `didChange` will read the same drain.

   `apply_edit` is a thin wrapper over `edit_raw`: the raw form mutates the rope
   and logs the `Edit`, the wrapper additionally records undo history. Undo and
   redo are the only callers of the raw form, so a new editing method gets undo
   by construction — and because history replays *through* `edit_raw`, an undo
   reaches tree-sitter and LSP as an ordinary incremental edit rather than a
   reason to reparse the file.
3. **Motions are data, not actions.** A [`Motion`] describes a destination;
   `Action::Move` goes there and `Action::Operate` deletes to there. Both
   resolve it through the same pure `fn(&self, Cursor) -> Cursor` functions on
   `Buffer`, so `w` and `dw` cannot drift apart. The cursor is a `Cursor` value
   rather than a field for the same reason an operator needs to ask where a
   motion *would* land without going there — and it is the shape visual mode
   (anchor + head) and split windows (one per view) will need.
4. **Registers are a ring, not named slots.** Vim makes you pick one of 36
   slots at yank time, which is the wrong moment — you rarely know yet whether
   a thing is worth keeping. Every `y`/`d`/`c`/`x` captures automatically into
   a 4096-deep ring, and the choice moves to paste time where a picker can
   search it. The ring lives on `Editor`, not `Buffer`: yanking in one file and
   pasting in another is the point.
5. **Undo is a tree, not a stack.** Undoing and then typing adds a second child
   to the current revision instead of discarding the first, so no keystroke can
   make earlier work unreachable. `u` / `Ctrl-R` walk one branch; the graph
   already stores what `g-` / `g+` would later traverse chronologically.

   Grouping happens at the command boundary in `Editor::apply`, not per
   mutation — `5x` is one undo step. Insert mode holds the group open until
   `Esc`, so a typing run undoes in one go, along with the `\n` that `o`
   inserted before it.
6. **Highlighting emits capture names, not styles.** `syntax.rs` produces
   `keyword` / `string` / `comment`; `tui/render.rs` maps those to colours. A GUI
   frontend writes its own table, and a theme file eventually replaces both.
   Producing `ratatui::Style` in the core would weld it to the terminal in the
   one place that is hardest to unpick.
7. **Rendering is viewport-bounded.** Frame cost scales with terminal height,
   not buffer size — including highlighting, which queries only the visible
   byte range.

## Known gaps

- Undo groups don't break on cursor movement inside insert mode; vim starts a
  new group when you arrow away mid-insert.
- No `g-` / `g+` / `:earlier`, so abandoned branches are stored but unreachable.
- Undo history is per-session; vim persists it with `'undofile'`.
- Display width counts chars, so CJK and combining chars misalign the cursor.
  Needs `unicode-width` and a grapheme walk.
- No horizontal scrolling — long lines clip.
- Single buffer, no window splits.
- No named registers (`"n`) and no system clipboard (`"+` / `"*`). See
  `docs/specs/registers.md`.
- Rust is the only grammar. Adding one is a line in `syntax.rs`, but each is a
  C library that costs build time.
- No tree-sitter injections, so code fences in markdown and JSX would not
  highlight. No indent queries, so no auto-indent. See
  `docs/specs/tree-sitter.md`.
- No text objects (`diw`, `ci"`, `da(`).
- `dw` uses a simplified form of vim's exclusive-motion rule: it stops at the
  end of the line rather than implementing the full "end in column 1" case.
- No git and no LSP, though both are the point — see
  [RECOMMENDATION.md](RECOMMENDATION.md). LSP hangs off `Editor::sync_syntax`,
  the same edit drain tree-sitter uses.

### Architectural, and cheaper to fix now than later

- **Tree-sitter is not optional, so building needs a C toolchain.** Grammars
  are C compiled by `cc`, which breaks minimal containers and makes
  cross-compilation harder. A Cargo feature would fix it: `Editor::syntax` is
  already `Option<Syntax>` and an unknown extension already renders as plain
  text, so the no-syntax path exists and works.
- **The config language is undecided**, and two tables are now waiting on it —
  the keymap in `input.rs` and the highlight colours in `tui/render.rs`. Both are
  hardcoded. RECOMMENDATION.md names this as the painful retrofit, and every
  feature added before deciding makes it slightly worse.
- **The core/frontend boundary is enforced by a test, not the compiler.** Fixed
  as far as it goes: there is a `lib.rs`, `input.rs` speaks `bee::key::Key`
  rather than crossterm's types, and rendering and event translation live in
  `src/tui/`. But a lib and a bin in one package share one dependency list, so
  nothing stops `editor.rs` from importing ratatui — except
  `tests/lib_boundary.rs`, which reads the library's modules and fails on any
  that name a terminal crate. Only a Cargo workspace would make that a compiler
  rule, and that is not worth its churn while there is one frontend.
- **The cursor lives on `Buffer`.** Two views of one file need two cursors, so
  window splits and visual mode's anchor both want it to move onto a view.
  `Cursor` is already a value type, which is the half of that work that was
  expensive.
- **`Editor::scroll` is a row index** and `scroll_to_cursor` takes a height in
  rows, which bakes in "the viewport is N whole lines". Soft wrap breaks that
  assumption, and so does any pixel-scrolling frontend.

## Next

LSP, which hangs off the same `pending_edits` drain that tree-sitter now uses —
`Editor::sync_syntax` is where `textDocument/didChange` goes. Before that, the
config-language decision (RECOMMENDATION.md, "what actually bites you" #1) is
still unmade and still cheap: the keymap in `input.rs` and the highlight table
in `tui/render.rs` are both waiting for it.

Previously, on tree-sitter: `pending_edits`
feeds `Tree::edit` + `Parser::parse` with the old tree; highlight queries then
map to styles in `tui/render.rs`. Deliberately *not* yet: a config language or a plugin
system.
