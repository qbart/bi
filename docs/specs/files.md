# `gf` — open a file by typing part of its name

The tree is for looking around. When you already know the name, walking a tree
to it is four keys too many.

## Status

**Built.** Opened by `gf` since the keys were rearranged — `gf` goes to a
file, `gb` goes to a buffer — and `Ctrl-P` no longer opens it from a text
window. A tree pane still answers `Ctrl-P`, because `gf` there is the walk
over the tree's own rows (`docs/specs/tree.md`) and the letter was taken.

## What it is

`gf` opens the picker over every file under the session's root, filtered as
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

Case-insensitive, and **ranked**: the list is sorted by how well each path
matches, not left in the walk's order. The first cut left the walk order —
directories first, then alphabetical — and it put `src/core/animation_curve.cpp`
above `src/main.cpp` for the query `main`, because both contain the letters in
order and `animation_curve` comes first alphabetically. A subsequence filter
without a ranking misses the point of a fuzzy finder: the letters appearing
*together* is most of what you were saying.

The scorer is the one the tree picker already had — 8 for a consecutive
character, 4 for one landing on a boundary, 1 anywhere else, shorter breaking
ties — with one repair it needed before a file list could trust it: it takes
the **best anchor for the first character**, not the first one. Scored greedily
from the first `m` in the text, `main` against `domain/main.rs` matches
`m`‑`a`‑`i`‑`n` scattered through `domain` and never sees the whole word
sitting after the slash. Every occurrence of the query's first character is a
candidate start, each is scored greedily from there, and the best one is the
answer. That is still not every alignment of every character — no fuzzy finder
people like does that — but it is the half of Sublime's ranking that pays for
itself.

The sort is stable, so equal scores keep the walk order, which stays the tie
someone chose.

## What is walked

From the session's root — one stored fact, resolved when the session opens
and read by the walk and the accept alike, so the chosen row always joins
the base its relative path was named against (`docs/specs/tree.md`, "The
root is the session's"). It used to be derived on demand, and two
derivations disagreed — the walk read the file's directory, the accept read
the working directory, and a picker that showed `material.cpp` then opened a
`material.cpp` that did not exist is this spec's founding bug. Deriving is
gone, not synchronised: there is nothing to keep agreeing when there is one
fact.

**Hidden entries are skipped**, the same rule the tree follows for the same
reason: `.git` alone would double the list.

**What the project ignores is skipped** — `.gitignore`, the repository's
`.git/info/exclude`, and every `.gitignore` from the repository root down,
with an ignored directory pruned rather than filtered. `docs/specs/`
`gitignore.md` is the whole of that, including which of the format's corners
are honoured and which two are not.

An earlier draft had a built-in list of likely directory names instead —
`target`, `node_modules`, `dist`, `build`. It is gone. It was a guess about
your project, and it was wrong in both directions: it missed whatever your
build tool is called this year, and it hid a checked-in `build/` full of
scripts you wanted to open. One mechanism, and it is the project's own.

**The walk stops at 20,000 files.** A picker over a home directory is a hang,
and a hang is worse than a truncated list that says it was truncated. That is
a backstop rather than a policy about files, which is why it survived the list
that was one.

## Opening

The chosen path goes through the same `:e` path everything else does, so a file
already open comes back as the buffer it already is rather than as a second
copy of it.

## Tests

- A subsequence finds a path: `sfr` matches `src/find/render.rs`.
- `main` ranks `src/main.cpp` above `src/core/animation_curve.cpp` — the run
  beats the scatter.
- `main` against `domain/main.rs` scores the word after the slash, not the
  letters inside `domain` — the anchor is the best one, not the first one.
- Equal scores keep the walk order.
- Terms still rule the register picker, so prose does not match everything.
- Hidden entries are absent from the walk, and so is everything the project's
  `.gitignore` names.
- The cap holds, and what is over it is dropped rather than hung on.
- Choosing a file shows it in the focused window; choosing one already open
  reuses its buffer.
- Nothing under the root at all says so rather than opening an empty overlay.
- A picked row opens under the root the list was walked from — the pinned
  session root, which a `:e` elsewhere between walk and accept cannot move.
- The root pinned at open is the working directory when the file sits under
  it, the file's own directory when it does not — `..` counted as escaping —
  and the working directory for a `[No Name]` buffer.
