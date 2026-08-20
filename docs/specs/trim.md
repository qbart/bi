# Trimming

Trailing whitespace is invisible, and every project has a hook, a linter or a
reviewer whose job is to notice it anyway. An editor that removes it on the way
out removes the hook, the lint and the review comment with it.

## Status

**Built.**

## The options

```
trim_on_write      = true    do any of this at all, on `:w`
trim_trailing      = true    spaces and tabs at the end of a line
trim_first_line    = true    blank lines at the top of the file
trim_last_line     = true    blank lines at the bottom of it
trim_final_newline = true    end the file with a newline if it has none
```

**Flat names with a prefix, not a table.** An earlier draft spelled these
`trim.trailing` and made `[options]` accept three TOML shapes for the same
setting — a dotted key, an inline table, an `[options.trim]` section. It was
one option in the file that behaved unlike every other, three code paths to
keep agreeing, and a `:set` name with a `.` in it that reads like a path. The
prefix does the grouping that a table was there to do, where a reader sees it,
and `[options]` goes back to being a flat list of names. Nesting is now an
error that names what you wrote.

`trim_on_write` is the master switch rather than a fifth thing to remember:
off, nothing below it happens, whatever the four say. It exists because "I want
bi to leave this repository's files exactly as it found them" is a real
sentence, and answering it should not mean turning four options off one at a
time.

**All five are on.** Earlier drafts shipped `last_line` and `final_newline`
off, on the grounds that blank lines at the bottom of a file are load-bearing
often enough — a heredoc, a fixture, a file whose format counts them — and that
a file without a final newline might be a file somebody meant. Both are real
and both are rare, and paying for them with untidy writes everywhere else is
the wrong trade. A write tidies the file; the project that wants otherwise says
`trim_last_line = false` once, in its config or its `.editorconfig`, and is
obeyed everywhere.

Note what this still does *not* do: a file that is nothing but blank lines is
left exactly as it was, `final_newline` only ever adds, and an empty file has
no line to terminate and stays empty.

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
format where whitespace is content. So bi ships `trim_trailing = false` as a
built-in default for `markdown`, in the same tiny table that gives a Makefile
its tabs (`docs/specs/options.md`).

An earlier draft had `trim_ft_blocklist = ["markdown"]`. It was dropped, and
the reason is worth writing down: options already resolve per file type, so a
list inside an option would be a *second* mechanism answering the question the
first one exists for — and the config that disables trimming in one and enables
it in the other would need a winner, with no obvious answer. `[filetype.x]` is
also finer-grained than a blocklist can be: markdown still gets its blank first
line trimmed, because only the option that would break it is off.

```toml
[filetype.markdown]
trim_trailing = true      # if you disagree with bi about this
```

## `.editorconfig`

Two of the format's properties are these options by another name, and they map
straight across:

```
trim_trailing_whitespace = true|false   → trim_trailing
insert_final_newline     = true|false   → trim_final_newline
```

Which is the layer that makes the defaults above affordable: a project that
disagrees states it once, in the file every other editor already reads, and is
obeyed. The two properties are the only ones anybody sets `false` on purpose,
and they are exactly the two that map.

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
  `trim_last_line` is off.
- A file of nothing but blank lines is left alone.
- `final_newline` adds one when there is none, changes nothing when there is,
  and leaves an empty file empty.
- A nested spelling — `[options.trim] trailing = false` — is an unknown option
  that names what was written, not a setting that quietly does nothing.
- `trim_on_write = false` means none of it happens.
- A markdown buffer keeps its two trailing spaces, and a `[filetype.markdown]`
  section can turn that back on.
- `.editorconfig`'s two properties reach the two options.
- The cursor is where it was, in text terms, after a trim that removed lines
  above it.
- The trim is one undo step, and `u` after `:w` restores the file.
- Nothing to trim writes no undo entry at all.
