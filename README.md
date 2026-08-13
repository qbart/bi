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
| `Esc` | back to normal mode |
| `:w` `:w <path>` `:q` `:q!` `:wq` `:{n}` | ex commands |

## Layout

| File | Holds |
|---|---|
| `buffer.rs` | rope, cursor, motions, the single mutation primitive |
| `editor.rs` | modes, the `Action` dispatch table, ex commands, scrolling |
| `input.rs` | keys → `Command`; counts and the operator-pending slot |
| `ui.rs` | viewport-bounded render pass |
| `main.rs` | terminal lifecycle, event loop |

## The three decisions this step locks in

1. **Cursor is a char index.** Byte and UTF-16 conversions happen at the edges
   (`Buffer::point_at`), never inside motion code. LSP wants UTF-16 columns,
   tree-sitter wants byte columns; neither leaks inward.
2. **One mutation primitive.** Every edit goes through `Buffer::apply_edit`,
   which records an `Edit` carrying old/new byte ranges and points — exactly
   `tree_sitter::InputEdit`. The rope field is private to enforce this. Nothing
   consumes `pending_edits` yet; `main.rs` drains it each frame.
3. **Rendering is viewport-bounded.** Frame cost scales with terminal height,
   not buffer size.

## Known gaps

- No undo. It wants an edit-log built on `Edit`, which is why that type exists.
- Display width counts chars, so CJK and combining chars misalign the cursor.
  Needs `unicode-width` and a grapheme walk.
- No horizontal scrolling — long lines clip.
- Single buffer, no window splits.
- `dd` is special-cased in `input.rs` rather than being `d` + a motion. Making
  the operator-pending slot parse a real motion is what unlocks `dw`, `d$`,
  `cw`, `yy`.

## Next

Tree-sitter, once undo and operator-pending motions are in. `pending_edits`
feeds `Tree::edit` + `Parser::parse` with the old tree; highlight queries then
map to styles in `ui.rs`. Deliberately *not* yet: a config language or a plugin
system.
