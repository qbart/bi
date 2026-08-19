# `.editorconfig`

A repository that has agreed how it is indented says so in a file at its root,
and every editor anyone on the project uses reads it. bi reading it is not a
feature so much as the absence of a bug: without it, the first thing bi does to
a well-run project is reindent a file the moment you touch it.

It is the fourth of the five layers in `docs/specs/options.md` — above what bi
thinks, above what your config says, above what the language asks for, and
below what you `:set` this session.

## Status

**Built**, for the properties that map onto options bi has. `insert_final_
newline` and `trim_trailing_whitespace` are named by the format and land with
the trim spec, which is the next one.

## What is read

```
root = true                 stop walking up here
indent_style = tab|space    → expandtab
indent_size = <n>|tab       → shiftwidth, and tab_width when it is unset
tab_width = <n>             → tab_width
```

**`indent_size` sets `tab_width` too**, unless the file also sets `tab_width`
— that is the format's own rule, and it is what makes `indent_size = 2` do
what someone writing it expects in a file that turns out to contain tabs.
`indent_size = tab` is the inverse: it means "whatever `tab_width` is", which
is exactly bi's `shiftwidth = 0`, so it resolves to that and needs no arithmetic.

**A value of `unset`** means "the editor's own default", so bi drops the
property rather than applying it — which leaves the layer below showing
through, which is what the word means.

Everything else in the file is ignored, and ignored *silently*: `charset`,
`end_of_line`, `max_line_length` and whatever else a project has written there
are not errors, they are properties for editors with features bi does not have
yet. bi is always UTF-8 and always writes `\n`, which is `charset = utf-8` and
`end_of_line = lf` whether or not anyone says so.

## How a file is found

Up from the file's own directory, one `.editorconfig` per level, stopping at
the filesystem root or at the first file that says `root = true`. The nearest
file wins, so applying them means applying the *farthest* first; within one
file, a later section beats an earlier one, which is the format's rule and the
reason sections are applied in the order they are written rather than in the
order they match.

Nothing is cached. The walk is a handful of `stat`s and it runs only when
options are resolved — a buffer opening, a `:set`, a `:reload`, a path
changing — never on a keystroke. The alternative is a cache that has to be
invalidated when a file bi does not have open changes, which is a bug waiting
for a Tuesday; `:reload` picking up an edit to `.editorconfig` falls out of not
having one.

A relative buffer path is resolved against the process's working directory,
because that is what a relative path already means to the `open` that read the
buffer in the first place. This is the one place the library reads an ambient
process value, and it is the same one `Buffer::open` reads implicitly.

## The glob

The format's own dialect, which is neither shell globbing nor a regular
expression:

```
*           anything except /
**          anything, / included
?           one character, not /
[abc]       one of those characters
[!abc]      one character that is none of them
{a,b,c}     any of the alternatives — nested, and they may contain any of this
{3..12}     any integer in the range, sign included
\*          a literal *
```

Three rules decide what a section name is matched *against*, and they are the
part everyone gets wrong:

- No `/` anywhere in the pattern — `*.py` — matches on the **file name alone**,
  at any depth. Written as `**/` in front of it.
- A leading `/` — `/Makefile` — is anchored to the directory the
  `.editorconfig` is in.
- A `/` anywhere else — `lib/**.js` — is also relative to that directory, and
  anchored there.

`{single}` with no comma in it is **literal**, braces included. That is the
format's rule, not an oversight: a lone `{x}` is far more likely to be a
filename than an alternation of one.

The matcher is bi's own, in `src/editorconfig.rs`, and it backtracks — patterns
are a handful of characters and the paths they run against are short, so the
simple thing is the right thing. The alternative was a dependency that walks
the filesystem itself, which would have put "where does a file live" inside a
library that deliberately takes that from its caller.

## The seam an embedder needs

```rust
/// Everything `.editorconfig` says about `path`, as a layer.
pub fn patch_for(path: &Path) -> OptionPatch;

/// The same, from files someone else read — nearest last.
pub fn patch_from(files: &[(PathBuf, String)], path: &Path) -> OptionPatch;
```

The second is the whole of the logic and touches no filesystem; the first is
twenty lines of walking up a directory tree on top of it. An embedder whose
files live in a database calls the second and does its own walking, exactly as
`ConfigSource` lets one supply `config.toml` from wherever it lives.

## What it does not do

**No `:editorconfig` command, and no switch to turn it off.** A project that
has one means it. If a file needs to disagree, `:set` outranks it, which is the
layer that exists for exactly that.

**No watching.** An `.editorconfig` edited in another window reaches an open
buffer on the next `:reload`, or the next time that buffer's options resolve
for any other reason. A file watcher is a whole subsystem, and this is the one
place that would want it today.

## Tests

- The glob, on its own: `*` stopping at `/`, `**` crossing it, a class, a
  negated class, nested alternation, a numeric range including negatives, an
  escaped `*`, and `{single}` staying literal.
- A pattern with no `/` matching at any depth; a leading `/` anchoring; an
  inner `/` anchoring.
- Sections apply in file order, so a later one wins.
- The nearest `.editorconfig` beats the one above it.
- `root = true` stops the walk, and a file above it is not read.
- `indent_size` sets `tab_width` when the file does not, and does not when it
  does.
- `indent_size = tab` resolves to `shiftwidth = 0`.
- `unset` leaves the layer below showing through.
- A file bi does not understand a word of changes nothing and reports nothing.
- End to end: a buffer opened under a directory with an `.editorconfig`
  resolves to what it says, `:set` still beats it, and the `[filetype.*]`
  section under it does not.
