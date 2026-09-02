# Tree-sitter

Incremental parsing, and syntax highlighting on top of it. The first change a
user actually *sees*, and the reason `Buffer::Edit` has existed since step 1.

## Status

**Built**, for thirty-five languages — see the table below. Markdown is block-level
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

**A capture styled the colour of nothing is a capture that did not happen.**
`operator` had an arm, and that arm was `Color::Gray` — ANSI 7, which is what
an unstyled cell already prints. So `&` in Go, `*` and `=` in C3 and `==`
everywhere were parsed, captured, ranked, styled and then painted invisible,
which from the outside is indistinguishable from a grammar that never matched
them. Operators are words of the language — `&x` and `x` are different
programs — so they now take a colour of their own. Brackets and separators
keep the neutral one deliberately: they are structure rather than meaning,
they are on every line, and colouring them is what makes a screen look busy.

The general form of this is worth stating, because a query test cannot catch
it: the syntax tests assert *capture names*, and the whole point of the
capture-name boundary is that they cannot know what colour a frontend picks.
So the theme needs its own assertion that a style is not the default one.

**And a capture no theme has heard of is the same failure**, arriving from the
other side. `@tag` had no entry in any of the four themes, so HTML element
names — `<a>`, `<div>`, the most-repeated token in the file — rendered in plain
foreground, parsed and captured and then thrown away. XML is what made it
impossible to ignore: an XML document is *almost entirely* tags, so the grammar
would have gone in, compiled, matched, passed its snippet test and produced a
screen indistinguishable from having no grammar at all. Tags now take the
`type` colour in all four themes — an element name names a kind of node, which
is what a type is — and `docs/specs/theme.md` carries the reasoning. The rule
this leaves behind: **adding a grammar means reading its query for capture
names the themes do not have**, and the ones that carry a file are the ones to
check first.

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

**A blanket `@variable` never outranks a real capture.** The tie-break above
ranks by dotted segments, and that is not enough on its own, because
`function` and `variable` both have one. `@variable` is not a claim about a
node — it is what a query calls an identifier when it had nothing better to
say — so it is ranked below everything, alongside `@none`, which explicitly
asks for no colour.

Without that, the tie falls through to pattern order, and the queries do not
agree on one. C writes `(identifier) @variable` first and its `@function`
patterns last, which is right for last-wins. Go writes them the other way
round — `@function` on line 17, the blanket on line 26 — because that query
targets `tree-sitter-highlight`, where the *first* pattern wins. Both are
correct upstream and they are exact opposites. `func main()` in Go came out a
variable, which is to say uncoloured, in the same editor where the same
construct in C came out a function.

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
- **Julia, templ and Crystal ship the file and do not export it.** The
  `.scm` is right there in the package; the crate has `LANGUAGE` and
  `NODE_TYPES` and either no `HIGHLIGHTS_QUERY` or one commented out. Since
  `include_str!` cannot reach into a dependency, all three are **vendored
  verbatim** into `src/queries/`, with the upstream and revision named in a
  header. All three are MIT. This is copying, not authoring, and the
  distinction is worth keeping: `hcl.scm` is bi's and can be wrong on bi's
  terms, while these three are somebody else's and should be re-copied rather
  than edited.
- **Slang and HLSL.** Both crates ship their `HIGHLIGHTS_QUERY` commented out.
  Both borrow C's. Not C++'s: Slang rejects it outright (no `auto` node), and
  HLSL *accepts* it and then matches nothing at all — the worse of the two
  failures, because a grammar that compiles its query and highlights nothing
  looks installed.

That last case is why `the_queries_bi_did_not_get_from_a_crate_produce_captures`
asserts a snippet comes back with captures in it, rather than asserting the
query merely compiles.

### And four ship only half of one

**C++'s query is a delta on C's, not a query.** Upstream `queries/cpp/highlights.scm`
opens with `; inherits: c`, and the crate ships only the C++ half — the
inheritance is a convention of the *editor* that loads the file, not of the
crate. Handed to `Query::new` alone it compiles, so nothing complains, and it
matches `auto`, the C++-only keywords, raw strings, and calls through a
`qualified_identifier`. Nothing else. No comment, no string, no number, no
`if`, no `return`, no `int`. A C++ file therefore rendered as a white wall with
`auto` and the odd `ns::fn` picked out of it — the same silent failure HLSL had,
arriving from the opposite direction: not a borrowed query that matches
nothing, but a shipped query that is only ever meant to be the second half of
one.

So `cpp` gets C's query concatenated in front of C++'s, which is exactly what
`; inherits:` means. The order is not cosmetic: ties between two patterns over
the same range fall through to the later pattern, so C's own
`(call_expression function: (identifier) @function)` has to keep beating its
own `(identifier) @variable`, and C++'s overrides have to land after both.

**And it is not one crate being careless.** Adding thirteen more grammars
turned up three more of the identical shape, which makes this the normal case
rather than the exception:

| grammar | inherits | alone it captures |
|---|---|---|
| C++ | `c` | `auto`, the C++-only keywords, raw strings |
| TypeScript | `ecma` | types. Thirty-five lines, no comment, no string, no keyword |
| SCSS | `css` | `@mixin`, `@include`, `@each`. No comment, no property, no number |
| templ | `go` | the templ tags. None of the Go the file is mostly made of |

All four get the base query concatenated in front of their own, which is what
`; inherits:` means, and all four are pinned by a snippet test.

The general lesson is that a crate shipping `HIGHLIGHTS_QUERY` is not evidence
that the query is whole. Only a snippet with captures in it is, which is why
the per-language snippet tests below are now the rule rather than a special
case for the hand-written ones.

### A predicate nobody runs is a guard that does not hold

`QueryCursor::matches` evaluates `#eq?`, `#match?` and `#any-of?` against the
text provider — which is the whole reason `RopeProvider` exists. Everything
else lands in `Query::general_predicates` and is **not applied**, and an
unapplied guard does not fail closed. The pattern simply matches everything it
can reach.

`#lua-match?` is Neovim's, and three of these queries use it. All three were
narrowing rules, so all three were inverted into blanket ones:

| pattern | meant | did |
|---|---|---|
| CMake `(line_comment) @keyword.directive` `"^#!/"` | colour a shebang | every comment magenta, not grey |
| CMake `(unquoted_argument) @constant` `"^[%u@][%u%d_]+$"` | colour `SHOUTING_CASE` | every argument yellow |
| GLSL `(identifier) @variable.builtin` `"^gl_"` | colour `gl_Position` | every identifier, `main` and `p` included |

So a pattern carrying a predicate tree-sitter will not run is **dropped
whole**. Each of the three refines something already captured more broadly, so
what is lost is a shade on a rare node and what is fixed is a wrong colour on a
common one — a CMake comment goes back to `@comment`. Running them properly
means a Lua-pattern engine or a regex dependency, and three patterns do not pay
for either. If a fourth arrives that is load-bearing rather than decorative,
that is the moment to reconsider, and `unevaluatable()` is the one place to
change.

**Dockerfile comes from `arborium-dockerfile`.** The obvious
`tree-sitter-dockerfile` is pinned to tree-sitter 0.20, and two versions of
tree-sitter cannot coexist in one binary — both declare `links = "tree-sitter"`,
so cargo refuses to resolve rather than letting a duplicate native runtime
link. Any future grammar has to be on 0.26 or it cannot be used at all.

**Crystal is a git dependency, and the crates.io crate is a trap.**
`tree-sitter-crystal 0.1.0` exists, resolves, compiles and exports a
`LANGUAGE`, and is a toy: a 229 KB parser against the real grammar's 44 MB,
with `puts`, `pp` and `p` as dedicated node types. Twenty-five lines of
ordinary Crystal — `module`, `require`, `struct`, a block, an interpolated
string, a macro — put **twelve `ERROR` nodes** through it. It ships no query
either, so nothing would have highlighted regardless.

The real one is `crystal-lang-tools/tree-sitter-crystal`, which nvim uses, and
it is git-only. Worth stating plainly because it is the third distinct way a
grammar fails quietly, after "no query" and "half a query": **the grammar
itself can be a stub**, and it announces that with neither an error nor a
missing symbol. The check that caught it was parsing real code and counting
`ERROR` nodes, which is now what any future grammar should have to survive.

**C3 is a git dependency**, `c3lang/tree-sitter-c3` — there is no crates.io
release. `cargo publish` refuses a crate with a git dependency, because a
published crate has to stay buildable from the registry alone and a git URL can
be force-pushed, renamed or deleted.

**Accepted, and not a debt.** bi is a library for its own frontends, not a
crate for strangers to depend on, so nothing is lost by never being on
crates.io — `cargo build`, `make install` and `cargo install --git` all work
untouched, and so does a frontend depending on bi by git. The `rev` is pinned
in `Cargo.toml` rather than left to `Cargo.lock`, so `cargo update` cannot
quietly move C3 onto whatever is on their default branch that day; taking a
newer grammar is then a visible edit.

### Languages

The table, by the key that reaches it:

| grammar | keys |
|---|---|
| Rust | `rs` |
| C | `c` `h` |
| C++ | `cpp` `cc` `cxx` `hpp` `hxx` `hh` |
| C3 | `c3` `c3i` `c3t` |
| Go | `go` |
| Python | `py` `pyi` |
| Lua | `lua` |
| Bash | `sh` `bash` `zsh` · `.bashrc` `.bash_profile` `.bash_login` `.bash_logout` `.bash_aliases` `.profile` `.zshenv` `.zprofile` `.zshrc` `.zlogin` `.zlogout` |
| CSS | `css` |
| GLSL | `glsl` `vert` `frag` `comp` |
| HLSL | `hlsl` |
| Slang | `slang` |
| HCL / Terraform | `tf` `tfvars` `hcl` |
| Dockerfile | `Dockerfile` |
| CMake | `cmake` · `CMakeLists.txt` |
| TOML | `toml` · `Cargo.lock` |
| YAML | `yaml` `yml` |
| JSON | `json` |
| INI | `ini` |
| Markdown | `md` `markdown` |
| Make | `mk` `mak` · `Makefile` `makefile` `GNUmakefile` |
| Ruby | `rb` `rake` `gemspec` · `Gemfile` `Rakefile` |
| Crystal | `cr` |
| HTML | `html` `htm` |
| XML | `xml` `xsd` `xsl` `xslt` `svg` `plist` `csproj` `vcxproj` `props` `targets` `xaml` |
| DTD | `dtd` |
| SCSS | `scss` |
| JavaScript | `js` `jsx` `mjs` `cjs` |
| TypeScript | `ts` `mts` `cts` `tsx` |
| Swift | `swift` |
| Java | `java` |
| C# | `cs` |
| R | `r` `R` |
| Julia | `jl` |
| templ | `templ` |

**`Makefile` is a name, not an extension**, and it was the case the "key on
the file name" decision was written for — the spec called it out by name three
steps before the grammar existed. `makefile` and `GNUmakefile` are the two
other spellings `make` itself looks for; `*.mk` still resolves as an extension.

**`.h` goes to C**, which is a coin-flip that has to land somewhere. A C++
project whose headers are `.h` gets a grammar that reads most of the file; a C
project handed the C++ grammar gets nothing better in exchange.

**Bash takes every startup file, not a favourite one.** A shell dotfile has no
extension by construction, so the whole-name arm is the only thing that can
reach it — and `.bashrc` alone was an arbitrary list of one. The rule is now
statable: every dotfile bash or zsh reads on the way in or out, which is the
eleven above. `.bash_aliases` is the one bash does not look for itself; it is
a convention `.bashrc` sources, which changes nothing about what is in it.

**And they all go to the bash grammar**, `.zsh*` included. zsh is not bash, but
there is no zsh grammar to prefer, and bash reads all but the exotic parts of a
normal rc file. `.profile` is sh rather than bash and arrives at the same place
for the same reason, from the other direction.

**`Gemfile` and `Rakefile` are Ruby**, and are the same case as `Makefile`
one row up: Ruby's ecosystem writes its build files as named, extensionless
Ruby, and `.rake` was already in the table while the file rake tasks actually
live in was not. A `Gemfile` is a Ruby DSL — `source`, `gem`, `group do` — and
was rendering plain. `Gemfile.lock` is *not* Ruby and gets no arm; see below.

**`Cargo.lock` is TOML, and `lock` is not a format.** The lockfile is the
second-most-opened file in a Rust checkout after the manifest beside it, and it
was rendering as plain text because its name ends in a suffix that means
nothing. So it is a whole-name key, not an extension one, and that is not a
shortcut: `yarn.lock` is its own bespoke format, `flake.lock` and
`package-lock.json` are JSON, and `Gemfile.lock` is none of the three — not
even Ruby, which is exactly why the arm above has to match `Gemfile` whole
rather than as a prefix. Mapping `lock` would be wrong for more files than it
is right for. `poetry.lock` and `uv.lock` *are* TOML and can have the same line
the day someone wants them.

**XML covers eleven suffixes because XML is a syntax, not a file type.** The
languages already here drag most of them in: a C# checkout has `.csproj`,
`.props` and `.targets`, a C++ one has `.vcxproj`, a .NET UI has `.xaml`, and
`.svg`, `.xsd`, `.xsl`/`.xslt` and `.plist` are XML that nobody calls XML. The
grammar does not care which, so the only cost of each is a line in the table
and the only cost of *omitting* one is a file that renders plain for no reason
the user can see.

**DTD is a second grammar and comes with the first.** `tree-sitter-xml` ships
both, and its `build.rs` compiles both parsers whether or not either is
referenced — so the compile time is already paid the moment XML is added, and
the only question left is whether the linker pulls the second symbol in. A
`.dtd` open next to the `.xml` it constrains is the whole use, one arm reaches
it, and the crate exports a query for it.

The original argument here was "Rust only": every grammar is a C library that
costs compile time and binary size, and the editor is written in Rust so Rust
is what gets dogfooded.

**That cost turned out to be the headline, and it is worth stating in full:**

| | release binary |
|---|---|
| Rust only | 4.75 MB |
| + TOML, YAML, JSON, INI, Markdown, CMake | 5.42 MB |
| + the other thirteen | 23.60 MB |
| + the thirteen after that | 49.84 MB |
| + XML and DTD | **49.97 MB** |

Thirty-three grammars is a **10× binary**, and the second thirteen cost more
than the first twenty put together. The jump is not evenly spread — the C-family
grammars dominate it, and C++, HLSL and Slang are each a large generated
parser table. A release build on a Raspberry Pi gained about three minutes.

The single worst offender is Crystal at 44 MB of generated C — more than twice
Swift, which was the previous record — and Ruby, Swift, C++ and TypeScript are
each in the same order. A release build on a Raspberry Pi is now about six
minutes.

**And XML is the counter-example that makes the shape of this clear: two
grammars for 132 KB**, a thousandth of what Crystal cost, on a relink that took
under three minutes. The price of a grammar is the size of the language it
parses, not the fact of having one — XML has a dozen productions and Crystal
has a macro system. So "thirty-five grammars" is not a number to budget
against, and a small language should never have to argue for its place here.
The 50 MB is owed by about six of these, which is also why the fix below is
Cargo features rather than a shorter table.

Nothing here is wrong, but it changes which deferred item matters most, and
"matters most" has become "is overdue". `Editor::syntax` is already
`Option<Syntax>` and an unknown name already renders as plain text, so **the
no-syntax path exists and works** — putting grammars behind Cargo features is a
mechanical change, and it is now the cheapest way to get the binary back. At
50 MB it should be the next thing done to this file, ahead of injections. The README already lists the C-toolchain
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
tree-sitter-cpp = "0.23"     # ships the `; inherits: c` half only — prepend C's query
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
tree-sitter-xml = "0.7"      # two grammars; XML_HIGHLIGHT_QUERY / DTD_HIGHLIGHT_QUERY
```

Most expose `LANGUAGE` and `HIGHLIGHTS_QUERY`, so a new grammar is one arm.
The exceptions are worth knowing before adding the next one: C, C++ and Bash
spell it `HIGHLIGHT_QUERY`; GLSL, HLSL and Slang name the language
`LANGUAGE_GLSL` and so on; markdown has two grammars and calls its query
`HIGHLIGHT_QUERY_BLOCK`; `arborium-dockerfile` exposes `language()` as a
function; and `tree-sitter-xml` is two grammars in one crate, naming them
`LANGUAGE_XML` / `LANGUAGE_DTD` and their queries `XML_HIGHLIGHT_QUERY` /
`DTD_HIGHLIGHT_QUERY` — a fifth spelling of the same constant. Both of its
queries are whole rather than an `; inherits:` delta, which after four in a row
is worth saying out loud.

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
- **every language highlights a snippet of itself**, and the assertion is on
  the *shape* — a comment, a keyword and a literal all come back captured.
  Compiling is not the bar: an inherited query compiles, and a borrowed one
  compiles, and both can match almost nothing. This is the test C++ did not
  have.
- a key and a value in each of TOML, YAML, JSON and INI get *different*
  captures — the tie-break above, from the side that shows
- `CMakeLists.txt` resolves by name, not extension — and so do `Cargo.lock`,
  `Gemfile` and `Rakefile`, each of which has to reach a grammar its suffix
  could never have found. `Gemfile.lock` is the negative case in the same test:
  a whole-name key must not leak to a name that merely starts with it.
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
