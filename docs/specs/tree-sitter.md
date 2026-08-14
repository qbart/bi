# Tree-sitter

Incremental parsing, and syntax highlighting on top of it. The first change a
user actually *sees*, and the reason `Buffer::Edit` has existed since step 1.

## Status

**Built**, for Rust only. Injections, indent queries and background parsing
remain deferred — see the end of this file.

## What is already in place

`Edit` is field-for-field `tree_sitter::InputEdit` — byte ranges plus
`Point`s carrying **byte** columns, which is the shape tree-sitter wants. Every
mutation funnels through `Buffer::edit_raw`, so `pending_edits` is a complete,
ordered log of what changed. Undo and redo replay through the same function, so
history traversal arrives as ordinary incremental edits rather than as a
special case that forces a full reparse.

`main.rs` drained `pending_edits` each frame and dropped it. That drain is now
`Editor::sync_syntax`, and it is where LSP `didChange` will hang too.

So the work is plumbing, not redesign. That was the bet.

## Where the tree lives

`Editor` owns it, not `Buffer`:

```rust
pub struct Editor {
    pub buffer: Buffer,
    pub syntax: Option<Syntax>,
    // …
}
```

Because `pending_edits` will have **two** consumers — tree-sitter now, LSP
`didChange` later — and whoever drains it destroys it for the other. One drain
point at the `Editor` level feeds both. If `Buffer` owned the tree and consumed
edits itself, LSP would need a second queue and the two could drift.

This is also the honest shape today: `Editor` holds exactly one buffer, so
`Editor` *is* the document. When buffers become plural, `buffer` + `syntax` +
`path` travel together into a `Document`, and that is the moment to make the
change — not before.

`buffer.rs` therefore keeps no tree-sitter dependency.

## Syncing

After each command, before rendering:

```rust
let edits = std::mem::take(&mut buffer.pending_edits);
for edit in &edits {
    tree.edit(&input_edit(edit));
}
parser.parse_with_options(&mut |byte, _| chunk_at(rope, byte), Some(&tree), None);
```

The edits have to be *taken* before the rope is borrowed, or the two borrows
overlap. That is why `sync_syntax` is a method on `Editor` rather than a couple
of inline lines at the call site.

Every edit must reach `Tree::edit` in order before the reparse — batching N
edits then parsing once is the intended usage, not a shortcut.

`parse_with_options` reads the rope in chunks rather than materialising a `String`, so
the cost stays proportional to the edit and not to file size. Copying the whole
buffer out on every keystroke would defeat the entire point of incremental
parsing.

**Synchronous for now.** Incremental reparse of a small edit is sub-millisecond
on normal files. A background parse thread is the escape hatch for very large
files, and adding it later does not disturb this interface.

## Highlighting

**Emit semantic capture names, never terminal styles.**

```rust
pub struct Span {
    pub start_byte: usize,
    pub end_byte: usize,
    /// Resolved through `Syntax::capture_name` — `keyword`, `string`,
    /// `comment`. An index rather than a `&str` so a `Span` carries no
    /// lifetime and no per-frame allocation.
    pub capture: u32,
}
```

`ui.rs` maps names to `ratatui::Style`. This is the boundary that keeps the
core frontend-agnostic: a terminal maps `keyword` to an ANSI colour, a GUI maps
it to a font weight and an RGB value it picks itself. Producing `Style` here
would weld the core to ratatui in the one place it is hardest to unpick, and a
theme system wants exactly this indirection anyway.

**Query only the visible byte range.** `QueryCursor::set_byte_range` restricted
to the rows being drawn. Frame cost stays bounded by terminal height, which is
the invariant the README's rendering decision commits to — highlighting a
10,000-line file to draw 40 rows would silently break it.

**Overlapping captures resolve narrowest-wins**, implemented by painting a
per-byte array over the visible range widest-first and run-length encoding the
result. Highlight queries nest, and the innermost match is the specific one.
The array is bounded by the viewport, so this stays cheap. Rolling this ourselves over
`QueryCursor` rather than using the `tree-sitter-highlight` crate: that crate
is stream-oriented and whole-document by design, which fights the viewport
restriction above. Revisit if injections make it worth it.

## Languages

Extension → grammar, table-driven so adding one is a line:

```rust
match ext {
    "rs" => Some(tree_sitter_rust::LANGUAGE),
    _ => None,
}
```

**Start with Rust only.** The editor is written in Rust, so it is what gets
dogfooded, and each grammar is a C library that costs compile time and binary
size. An unknown extension means no `Syntax` and plain text — never an error.

## Dependencies, and the cost of them

```toml
tree-sitter = "0.26"
tree-sitter-rust = "0.24"
streaming-iterator = "0.1"   # QueryCursor::matches is a streaming iterator
```

These are the **first non-pure-Rust dependencies**. Grammars are C compiled by
`cc`, which means a C toolchain becomes a build requirement and
cross-compilation gets meaningfully harder. That is a real cost and worth
naming rather than discovering. It is also exactly the trade RECOMMENDATION.md
already accepted; `gitoxide` and pure-Rust alternatives have no equivalent here.

`Edit`'s `#[allow(dead_code)]` comes off.

## Testing

The strongest available invariant, and the one worth building first:

- **an incremental reparse equals a fresh parse.** Apply a sequence of edits
  incrementally, parse the same final text from scratch, assert the trees have
  the same S-expression. This catches a wrong `InputEdit` — the single most
  likely bug, and one that otherwise shows up as mysterious mis-highlighting
  much later.
- the same, with an undo in the middle, since undo replays through `edit_raw`
- multi-byte text: an edit after `é` reports byte offsets, not char offsets
  (`Edit` already has a test pinning this; extend it through to the tree)

Highlighting:

- a snippet yields expected capture names at expected byte ranges
- querying a byte range returns only spans within it
- nested captures resolve to the narrowest
- a file with no known extension highlights as plain text and does not error

## Deferred

**Injections** — markdown with embedded code, JSX, Vue SFCs. RECOMMENDATION.md
names this as where most of the complexity lives: per-region parsers and
incremental reparse across injection boundaries. It needs the single-language
path working first.

**Indent and textobject queries.** Tree-sitter's `indents.scm` gives real
auto-indent, and `textobjects.scm` gives `dif` / `daf` — delete inside/around a
function. The latter is worth noting because it lands directly on the operator
grammar that already exists: a text object is another range source alongside a
motion.

**Background parsing** for large files.

**Theme configuration** — the capture-name table lives in `ui.rs` until the
config language decision (RECOMMENDATION.md, "what actually bites you" #1) is
made.
