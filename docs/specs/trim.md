# Trimming

Trailing whitespace is invisible, and every project has a hook, a linter or a
reviewer whose job is to notice it anyway. An editor that removes it on the way
out removes the hook, the lint and the review comment with it.

## Status

**Built.**

## The options

```
trim.on_write      = true    do any of this at all, on `:w`
trim.trailing      = true    spaces and tabs at the end of a line
trim.first_line    = true    blank lines at the top of the file
trim.last_line     = true    blank lines at the bottom of it
trim.final_newline = false   end the file with a newline if it has none
```

Dotted names, inside `[options]`, because `[options]` **is** the `:set`
namespace and there is exactly one of those — `:set trim.trailing false` and
`trim.trailing = false` reach one setting, the same promise every other option
makes. TOML spells the same thing three ways (`trim.trailing = false`, an
inline table, or an `[options.trim]` section) and all three arrive here.

`trim.on_write` is the master switch rather than a fifth thing to remember: off,
nothing below it happens, whatever the four say. It exists because "I want bi
to leave this repository's files exactly as it found them" is a real sentence,
and answering it should not mean turning four options off one at a time.

**`trim.last_line` and `trim.first_line` both default to on**, because they are
the same accident at the two ends of a file. An earlier draft had `last_line`
off, on the grounds that blank lines at the bottom are load-bearing often
enough — a heredoc, a fixture, a file whose format counts them — that removing
them would be bi editing data. It is a real case and it is rare, and paying for
it with a run of empty lines in every other diff is the wrong trade: the file
that counts its trailing newlines says `trim.last_line = false` once, in the
project's config, and everyone else gets a write that tidies both ends.

Note what this still does *not* do: a file that is nothing but blank lines is
left exactly as it was, and the last line keeps whatever terminator it had.

**`trim.final_newline` defaults to off**, which is not what `.editorconfig`
usually says, and deliberately: bi's job out of the box is to write the file
you have. A project that wants the newline says so in its `.editorconfig` and
gets it; nobody is surprised by a diff they did not ask for.

## What each one does

**`trailing`** — the run of spaces and tabs before a line's terminator, on
every line. A line of nothing but whitespace becomes empty, which is why this
runs before the two below and why they see it as blank.

**`first_line`** — blank lines at the top, in one edit. A file that is *only*
blank lines is left alone: there would be nothing left, and "your file is now
empty" is a bad thing to learn from a `:w`.

**`last_line`** — blank lines at the bottom, in one edit, leaving the last
line with whatever terminator it already had.

**`final_newline`** — one `\n` at the end when the file does not end in one.
It only ever adds. Removing extra newlines at the end is `last_line`'s job, and
keeping the two separate is what lets a project ask for one without the other.

## Markdown is exempt, and not by a list

Two trailing spaces in Markdown are a hard line break — actual syntax, in a
format where whitespace is content. So bi ships `trim.trailing = false` as a
built-in default for `markdown`, in the same tiny table that gives a Makefile
its tabs (`docs/specs/options.md`).

An earlier draft had `trim.ft_blocklist = ["markdown"]`. It was dropped, and
the reason is worth writing down: options already resolve per file type, so a
list inside an option would be a *second* mechanism answering the question the
first one exists for — and the config that disables trimming in one and enables
it in the other would need a winner, with no obvious answer. `[filetype.x]` is
also finer-grained than a blocklist can be: markdown still gets its blank first
line trimmed, because only the option that would break it is off.

```toml
[filetype.markdown]
trim.trailing = true      # if you disagree with bi about this
```

## `.editorconfig`

Two of the format's properties are these options by another name, and they map
straight across:

```
trim_trailing_whitespace = true|false   → trim.trailing
insert_final_newline     = true|false   → trim.final_newline
```

Which is the layer that makes the conservative defaults above the right ones: a
project that has an opinion states it and is obeyed, and one that does not gets
an editor that does not rewrite its files.

## When it happens, and what it costs you

On `:w` and `:wa`, before the bytes go out — so what is written and what is in
the buffer are the same text, which is the property that keeps "modified" and
the undo history honest.

**The trim is its own undo step.** One `u` after a `:w` puts the whitespace
back and nothing else, rather than reverting the edit you made before saving.
That also means the buffer reads as modified again, which is correct: it is.

**Cursors follow the text.** Every selection is mapped through the edits rather
than clamped after them — a cursor on line 400 does not jump because three
blank lines went from the top of the file. A cursor sitting *inside* removed
whitespace lands where that whitespace was, which is the end of its line. Other
windows on the same buffer follow through `settle`, like every other edit.

## Tests

- Trailing spaces and tabs go, on every line, and a whitespace-only line
  becomes empty.
- Blank lines go from both ends by default, and trailing ones stay when
  `trim.last_line` is off.
- A file of nothing but blank lines is left alone.
- `final_newline` adds one when there is none and changes nothing when there is.
- `trim.on_write = false` means none of it happens.
- A markdown buffer keeps its two trailing spaces, and a `[filetype.markdown]`
  section can turn that back on.
- `.editorconfig`'s two properties reach the two options.
- The cursor is where it was, in text terms, after a trim that removed lines
  above it.
- The trim is one undo step, and `u` after `:w` restores the file.
- Nothing to trim writes no undo entry at all.
