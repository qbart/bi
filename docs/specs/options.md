# Options, per file

`:set expandtab false` was a switch for the whole session, which is fine until
the session has a Makefile and a Python file open at once — and every session
eventually does. A tab is not a preference in a Makefile, it is the syntax; two
spaces is not a preference in a Ruby file, it is what everyone else in the
repository has already agreed. An editor with one answer for both is an editor
that is wrong in one of the two windows.

So an option resolves **per buffer**. `Options` stops being a value the session
holds and becomes a value each buffer arrives at, from four layers that each
know something the others do not.

## Status

**Built**, without the `.editorconfig` layer, which is the next spec
(`editorconfig.md`) and slots in where this one says it does.

## The layers

From weakest to strongest:

```
1  bi's defaults          src/config/default.toml
2  your config            [options] in config.toml
3  what the language is   built-in filetype defaults, then [filetype.<name>]
4  what the project is    .editorconfig            (next spec)
5  what you just said     :set, this session
```

Each layer is a **patch**, not a configuration: it names the options it has an
opinion about and says nothing about the rest. That is the whole mechanism —
[`OptionPatch`](../../src/config/mod.rs) is an ordered list of name/value pairs
applied through `Options::set`, the one function where a name becomes a field.
No layer needs a type of its own, no layer can hold an option the others
cannot, and an option added in a later version is available to every layer the
day it exists.

**Why 3 sits above 2.** A `Makefile` needs tabs whatever your `expandtab` says,
because a Makefile with spaces does not run. The built-in table is deliberately
tiny — it is for what a language *requires* or has *universally settled*, not
for taste:

```
make    expandtab = false, tab_width = 8
go      expandtab = false
```

`[filetype.<name>]` in your config is the same layer and applied after it, so
the built-ins are a default rather than a law:

```toml
[filetype.go]
tab_width = 4          # gofmt writes tabs; how wide they look is yours

[filetype.python]
shiftwidth = 4
```

The name is the one `src/syntax.rs` gives the file — `rust`, `make`,
`markdown`, `csharp` — which is the same name the grammar is chosen by. That is
deliberate: two tables answering "what kind of file is this" would eventually
disagree, so there is one, and `filetype()` is now what it answers with.

**Why 5 sits above 4.** `:set tab_width 8` has to do something. If the
project's `.editorconfig` outranked it, the option would silently do nothing in
exactly the repositories that have their act together, and the only way to find
out would be to read the source. Explicit beats implicit; the price is that a
`:set` follows you into the next file, which is what "session" means and what
`:set` has always done.

## What this changes

`Options` is unchanged as a type — it is still the `:set` namespace, one field
per option — but there are now several of them: one on the session, and one
resolved per open buffer.

```rust
struct BufferEntry {
    /// The session's options with this file's layers laid over them.
    options: Options,
}
```

The session's copy is layers 1, 2 and 5, and it is what a buffer's resolution
starts from and what a session-wide question is asked of. Per-buffer copies are
recomputed — never patched in place — whenever a layer under them moves: a
config load or `:reload`, a `:set`, a buffer opening, or a path changing under
`:w some/other/name`. Recomputing a handful of `Options` is cheaper than
working out which buffers a change could have reached, and it cannot go stale.

`View` and `Pane` both carry `&Options` now, so the editing commands and the
renderer read the buffer's rather than the session's. That is the part that
actually makes the feature real: `>` in one window and `>` in another can move
by different amounts, and the tab in a Makefile can be eight columns wide while
the one in the Go file beside it is four.

## What is *not* per file

Nothing, structurally — every option resolves the same way. But only some
options have a layer above the session that says anything: the indentation
four today, and trimming next. `theme` is in the same table and could in
principle be set per filetype; it would be a strange thing to want, and nothing
stops it, which is the right amount of policy for a mechanism to have.

## Tests

- The layers stack in order: a config `expandtab = true` loses to the `make`
  built-in, which loses to `[filetype.make]`, which loses to `:set`.
- A buffer with no filetype gets the session's options unchanged.
- Two buffers of different types, open at once, resolve differently — which is
  the whole point and is the test that would have failed before this.
- `:set` reaches buffers that are already open, not just the next one.
- `:reload` re-resolves every buffer, so removing a `[filetype.go]` section
  from the config takes effect on the file that is already open.
- Renaming a buffer with `:w other.go` re-resolves it.
- An unknown option or a bad value inside `[filetype.x]` is one reported
  problem, not a refusal to start — the same rule `[options]` follows.
