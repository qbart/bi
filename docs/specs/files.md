# `Ctrl-P` — open a file by typing part of its name

The tree is for looking around. When you already know the name, walking a tree
to it is four keys too many.

## Status

**Built.**

## What it is

`Ctrl-P` opens the picker over every file under the session's root, filtered as
you type, and opens the one you choose in the focused window. It is the same
overlay the register ring and `:ls` use — the widget was written to be shared,
and this is its third client.

Paths are shown relative to the root, because that is what you would have typed
and because a column of identical prefixes says nothing.

## Matching is a subsequence here, and terms everywhere else

The register picker matches by whitespace-separated *terms*, all of which must
appear somewhere: register entries are prose and code, where "these letters
appear in order" matches nearly everything.

A file list is the opposite case. `sfr` should find `src/find/render.rs`, which
is exactly the thing terms cannot do, and the same laxness that would be
useless over prose is what makes a path list usable. So the picker asks its
kind which rule to apply, and files are the kind that gets subsequences.

Case-insensitive, and the order is the walk's rather than a ranking. A ranking
— fzf's, which weighs word boundaries and consecutive runs — is a real
improvement and a bigger piece of work; the walk order is directories first and
then alphabetical, which is at least an order somebody chose.

## What is walked

From the session's root — the tree's root if there is one, else the directory
of the file bi was opened on.

**Hidden entries are skipped**, the same rule the tree follows for the same
reason: `.git` alone would double the list.

**A built-in list of build directories is skipped** — `target`, `node_modules`,
`.git`, `dist`, `build`, `vendor`, `__pycache__`. Not because they are not
files, but because they are files nobody opens by name, and one of them can be
larger than everything else put together.

**The walk stops at 20,000 files.** A picker over a home directory is a hang,
and a hang is worse than a truncated list that says it was truncated.

`.gitignore` is *not* read, and that is the one thing here worth flagging: it
is the right answer and it needs a gitignore matcher (its own dialect,
negations, directory-only patterns, one file per directory) plus the git
support bi does not have yet. The built-in list covers the cases that actually
bite until then.

## Opening

The chosen path goes through the same `:e` path everything else does, so a file
already open comes back as the buffer it already is rather than as a second
copy of it.

## Tests

- A subsequence finds a path: `sfr` matches `src/find/render.rs`.
- Terms still rule the register picker, so prose does not match everything.
- Hidden entries and build directories are absent from the walk.
- The cap holds, and what is over it is dropped rather than hung on.
- Choosing a file shows it in the focused window; choosing one already open
  reuses its buffer.
- Nothing under the root at all says so rather than opening an empty overlay.
