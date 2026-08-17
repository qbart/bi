# Tree-sitter

Incremental parsing, and syntax highlighting on top of it. The first change a
user actually *sees*, and the reason `Buffer::Edit` has existed since step 1.

## Status

**Built**, for twenty languages — see the table below. Markdown is block-level
only until injections land. Injections, indent queries and background parsing
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

`tui/render.rs` maps names to `ratatui::Style`. This is the boundary that keeps the
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

**Two captures over the identical range is not nesting**, and the order
tree-sitter yields them in carries no meaning, so the tie is broken explicitly:
the name with more dotted segments wins, and failing that the later pattern.
This is not hypothetical — a JSON key is captured as both `string.special.key`
and `string`, and a YAML key as both `property` and `string`. Worse, the two
queries disagree about which order to write them in, so any rule based on
pattern order alone fixes one language and breaks the other. Left unresolved,
keys take the colour of ordinary string values and a config file renders as one
green wall.

**`@spell` and `@nospell` are dropped.** They mark where a spellchecker should
look and say nothing about colour, but INI and CMake both hang one on the same
node as `@comment`. Letting them through leaves comments competing against a
capture no theme has an entry for, and comments come out unstyled.
The array is bounded by the viewport, so this stays cheap. Rolling this ourselves over
`QueryCursor` rather than using the `tree-sitter-highlight` crate: that crate
is stream-oriented and whole-document by design, which fights the viewport
restriction above. Revisit if injections make it worth it.

## Languages

File name → grammar, table-driven so adding one is a line:

```rust
if file == "CMakeLists.txt" { return Some(cmake()) }
match file.rsplit('.').next() {
    "rs"   => tree_sitter_rust,
    "toml" => tree_sitter_toml_ng,
    // …
    _ => None,
}
```

**The key is the file name, not the extension.** `Syntax::new` takes
`Cargo.toml` or `CMakeLists.txt`, tries whole names first, and falls back to
the text after the last dot. A bare `"rs"` is a name with no dot in it, so it
still resolves as an extension and the old callers still work. This exists
because build files are named rather than suffixed — CMake is written into
`CMakeLists.txt` far more often than into `*.cmake`, and `Makefile`,
`Dockerfile` and `.gitconfig` will want the same door.

An unknown name means no `Syntax` and plain text — never an error.

### Not every grammar ships a query

Three of them do not, and the failure is invisible: `Query::new(..).ok()?`
turns a missing or incompatible query into plain text with no message.

- **HCL / Terraform.** `tree-sitter-hcl` publishes the parser and excludes the
  query files from the package. The upstream repository has them, but
  `include_str!` cannot reach into a dependency, so bi ships its own —
  `src/queries/hcl.scm`, written against that crate's `node-types.json`. It is
  the only query here bi authors.
- **Slang and HLSL.** Both crates ship their `HIGHLIGHTS_QUERY` commented out.
  Both borrow C's. Not C++'s: Slang rejects it outright (no `auto` node), and
  HLSL *accepts* it and then matches nothing at all — the worse of the two
  failures, because a grammar that compiles its query and highlights nothing
  looks installed.

That last case is why `the_queries_bi_did_not_get_from_a_crate_produce_captures`
asserts a snippet comes back with captures in it, rather than asserting the
query merely compiles.

**Dockerfile comes from `arborium-dockerfile`.** The obvious
`tree-sitter-dockerfile` is pinned to tree-sitter 0.20, and two versions of
tree-sitter cannot coexist in one binary — both declare `links = "tree-sitter"`,
so cargo refuses to resolve rather than letting a duplicate native runtime
link. Any future grammar has to be on 0.26 or it cannot be used at all.

**C3 is a git dependency**, `c3lang/tree-sitter-c3` — there is no crates.io
release. That is a real cost: it pins a commit rather than a version, and it
would block publishing bi to crates.io.

### Languages

The table, by the key that reaches it:

| grammar | keys |
|---|---|
| Rust | `rs` |
| C | `c` `h` |
| C++ | `cpp` `cc` `cxx` `hpp` `hxx` `hh` |
| C3 | `c3` `c3i` |
| Go | `go` |
| Python | `py` `pyi` |
| Lua | `lua` |
| Bash | `sh` `bash` `zsh` · `.bashrc` `.zshrc` |
| CSS | `css` |
| GLSL | `glsl` `vert` `frag` `comp` |
| HLSL | `hlsl` |
| Slang | `slang` |
| HCL / Terraform | `tf` `tfvars` `hcl` |
| Dockerfile | `Dockerfile` |
| CMake | `cmake` · `CMakeLists.txt` |
| TOML | `toml` |
| YAML | `yaml` `yml` |
| JSON | `json` |
| INI | `ini` |
| Markdown | `md` `markdown` |

**`.h` goes to C**, which is a coin-flip that has to land somewhere. A C++
project whose headers are `.h` gets a grammar that reads most of the file; a C
project handed the C++ grammar gets nothing better in exchange.

**`.zshrc` goes to the bash grammar.** zsh is not bash, but there is no zsh
grammar to prefer, and bash reads all but the exotic parts of a normal rc file.

The original argument here was "Rust only": every grammar is a C library that
costs compile time and binary size, and the editor is written in Rust so Rust
is what gets dogfooded.

**That cost turned out to be the headline, and it is worth stating in full:**

| | release binary |
|---|---|
| Rust only | 4.75 MB |
| + TOML, YAML, JSON, INI, Markdown, CMake | 5.42 MB |
| + the other thirteen | **23.60 MB** |

Twenty grammars is a 5× binary. The jump is not evenly spread — the C-family
grammars dominate it, and C++, HLSL and Slang are each a large generated
parser table. A release build on a Raspberry Pi gained about three minutes.

Nothing here is wrong, but it changes which deferred item matters most.
`Editor::syntax` is already `Option<Syntax>` and an unknown name already
renders as plain text, so **the no-syntax path exists and works** — putting
grammars behind Cargo features is a mechanical change, and it is now the
cheapest way to get the binary back. The README already lists the C-toolchain
version of this argument under "cheaper to fix now than later".

What bought the 18 MB is that these are the formats an editor sits in front
of: its own config is TOML, its build is CMake, its shell is bash.

**Markdown runs its block grammar only.** `tree-sitter-md` is *two* parsers:
a block grammar for headings, fences, lists and quotes, and a separate inline
grammar for emphasis, links and code spans. Reaching the second one means
injections, which are still deferred below. Block structure is most of what
the eye uses, so half is worth having; the fence of a code block highlights
while its contents do not.

## Dependencies, and the cost of them

```toml
tree-sitter = "0.26"
streaming-iterator = "0.1"   # QueryCursor::matches is a streaming iterator

tree-sitter-rust = "0.24"
tree-sitter-toml-ng = "0.7"  # the maintained TOML grammar; tree-sitter-toml is on an ABI from 0.20
tree-sitter-yaml = "0.7"
tree-sitter-json = "0.24"
tree-sitter-ini = "1.4"
tree-sitter-md = "0.5"       # LANGUAGE is the block grammar; INLINE_LANGUAGE waits on injections
tree-sitter-cmake = "0.7"
tree-sitter-go = "0.25"
tree-sitter-c = "0.24"
tree-sitter-cpp = "0.23"
tree-sitter-lua = "0.5"
tree-sitter-bash = "0.25"
tree-sitter-css = "0.25"
tree-sitter-python = "0.25"
tree-sitter-hcl = "1.1"      # parser only — the query is src/queries/hcl.scm
tree-sitter-glsl = "0.2"
tree-sitter-hlsl = "0.2"     # query commented out upstream; borrows C's
tree-sitter-slang = "0.3"    # ditto
arborium-dockerfile = "2.18" # tree-sitter-dockerfile is stuck on tree-sitter 0.20
tree-sitter-c3 = { git = "https://github.com/c3lang/tree-sitter-c3" }  # no crates.io release
```

Most expose `LANGUAGE` and `HIGHLIGHTS_QUERY`, so a new grammar is one arm.
The exceptions are worth knowing before adding the next one: C, C++ and Bash
spell it `HIGHLIGHT_QUERY`; GLSL, HLSL and Slang name the language
`LANGUAGE_GLSL` and so on; markdown has two grammars and calls its query
`HIGHLIGHT_QUERY_BLOCK`; and `arborium-dockerfile` exposes `language()` as a
function.

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
- a file with no known name highlights as plain text and does not error

Per grammar, because a grammar fails *quietly*:

- **every name in the table yields a parser.** `Query::new(..).ok()?` turns a
  query that will not compile against the current tree-sitter into plain text
  with no message anywhere. A grammar bumped past an ABI it shares with us
  would otherwise be noticed by a user, not by the suite.
- a key and a value in each of TOML, YAML, JSON and INI get *different*
  captures — the tie-break above, from the side that shows
- `CMakeLists.txt` resolves by name, not extension
- no `@spell` or `@nospell` ever reaches the frontend

## Deferred

**Injections** — markdown with embedded code, JSX, Vue SFCs. RECOMMENDATION.md
names this as where most of the complexity lives: per-region parsers and
incremental reparse across injection boundaries. It needs the single-language
path working first.

Markdown now gives this a concrete first customer rather than a hypothetical
one, and a cheap one: `tree-sitter-md` ships `INJECTION_QUERY_BLOCK`, and the
first region to light up is markdown's *own* inline grammar, which needs no
language lookup at all. Fenced code blocks — a `rust` fence resolving through
the same table `language_for` already is — follow from the same machinery.
`Syntax` holding one `Tree` is the thing that has to give.

**Indent and textobject queries.** Tree-sitter's `indents.scm` gives real
auto-indent, and `textobjects.scm` gives `dif` / `daf` — delete inside/around a
function. The latter is worth noting because it lands directly on the operator
grammar that already exists: a text object is another range source alongside a
motion.

**Background parsing** for large files.

**Theme configuration** — the capture-name table lives in `tui/render.rs` until the
config language decision (RECOMMENDATION.md, "what actually bites you" #1) is
made.
