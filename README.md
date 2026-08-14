# bee

A batteries-included modal editor. Tree-sitter, git, and LSP are meant to be
built in, not plugins.

Status: **step 1** — the core loop only. See
[RECOMMENDATION.md](RECOMMENDATION.md) for why the stack is what it is.

```sh
cargo run -- <file>
cargo test
```

## Keys

| | |
|---|---|
| `h j k l` `w b` `0 ^ $` | motions (accept counts: `5j`) |
| `gg` `G` `{n}G` | first line, last line, line *n* |
| `i a I A` | insert before/after cursor, at line start/end |
| `o O` | open line below/above |
| `x` `dd` | delete char, delete line |
| `u` `Ctrl-R` | undo, redo (accept counts: `3u`) |
| `Esc` | back to normal mode |
| `:w` `:w <path>` `:q` `:q!` `:wq` `:{n}` | ex commands |

## Layout

| File | Holds |
|---|---|
| `buffer.rs` | rope, cursor, motions, the single mutation primitive |
| `history.rs` | the undo tree: revisions, branching, invertible `Change`s |
| `editor.rs` | modes, the `Action` dispatch table, ex commands, scrolling |
| `input.rs` | keys → `Command`; counts and the operator-pending slot |
| `ui.rs` | viewport-bounded render pass |
| `main.rs` | terminal lifecycle, event loop |

## The four decisions this step locks in

1. **Cursor is a char index.** Byte and UTF-16 conversions happen at the edges
   (`Buffer::point_at`), never inside motion code. LSP wants UTF-16 columns,
   tree-sitter wants byte columns; neither leaks inward.
2. **One mutation primitive.** Every edit goes through `Buffer::apply_edit`,
   which records an `Edit` carrying old/new byte ranges and points — exactly
   `tree_sitter::InputEdit`. The rope field is private to enforce this. Nothing
   consumes `pending_edits` yet; `main.rs` drains it each frame.

   `apply_edit` is a thin wrapper over `edit_raw`: the raw form mutates the rope
   and logs the `Edit`, the wrapper additionally records undo history. Undo and
   redo are the only callers of the raw form, so a new editing method gets undo
   by construction — and because history replays *through* `edit_raw`, an undo
   reaches tree-sitter and LSP as an ordinary incremental edit rather than a
   reason to reparse the file.
3. **Undo is a tree, not a stack.** Undoing and then typing adds a second child
   to the current revision instead of discarding the first, so no keystroke can
   make earlier work unreachable. `u` / `Ctrl-R` walk one branch; the graph
   already stores what `g-` / `g+` would later traverse chronologically.

   Grouping happens at the command boundary in `Editor::apply`, not per
   mutation — `5x` is one undo step. Insert mode holds the group open until
   `Esc`, so a typing run undoes in one go, along with the `\n` that `o`
   inserted before it.
4. **Rendering is viewport-bounded.** Frame cost scales with terminal height,
   not buffer size.

## Known gaps

- Undo groups don't break on cursor movement inside insert mode; vim starts a
  new group when you arrow away mid-insert.
- No `g-` / `g+` / `:earlier`, so abandoned branches are stored but unreachable.
- Undo history is per-session; vim persists it with `'undofile'`.
- Display width counts chars, so CJK and combining chars misalign the cursor.
  Needs `unicode-width` and a grapheme walk.
- No horizontal scrolling — long lines clip.
- Single buffer, no window splits.
- `dd` is special-cased in `input.rs` rather than being `d` + a motion. Making
  the operator-pending slot parse a real motion is what unlocks `dw`, `d$`,
  `cw`, `yy`.

## Next

Tree-sitter, once operator-pending motions are in. `pending_edits`
feeds `Tree::edit` + `Parser::parse` with the old tree; highlight queries then
map to styles in `ui.rs`. Deliberately *not* yet: a config language or a plugin
system.
