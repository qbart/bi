# Finding, and replacing, across the project

`/` searches the buffer. `:find` searches the project, and puts what it found
in a pane you can leave open beside the file you are editing. `:replace` shows
you every rewrite before any of them happens, and lets you take them one at a
time or all at once.

## Status

**Built.**

```
:find needle           every match under the project root
:find src/ needle      the same, under src/ only
:find~ \bfn \w+        the pattern read as a regular expression
:replace /old/new/     find old, preview replacing it with new
:replace //new/        preview replacing what the pane already shows
:replace~ /f(\w+)/$1/  the regex twin — $1 is the first group
:results               bring the last results pane back
```

## ripgrep as a library

`grep-searcher` for the line walk, `grep-regex` for the pattern, `ignore` for
the directory walk that already knows what a `.gitignore` is. Not an `rg`
subprocess: bi's core is a library for frontends that are not this one, and a
library that shells out to a binary the host may not have is a library that
works on one machine.

`src/find_in_files.rs` searches and knows nothing else — no buffer, no window.
The editor joins it to those, the same split every other module here makes.

**`fixed_strings` rather than `build_literals`.** The literal builder wraps the
pattern in a group without escaping it, so `:find a(` came back as "unclosed
group" — a regex error about a search that was never meant to be a regex. One
`build` either way also keeps the offsets a match reports meaning the same
thing in both modes.

**A literal by default, a regex by another command name.** `:find fn main()`
has to search for `fn main()`. There are no flags on the line because the whole
argument is the pattern: a `-r` would be a pattern you could not search for.

The regex name is **`:find~`** — the `~` reads as "approximately", it sits on
the name rather than in the argument, and it is one keystroke on top of the
command it varies. It was `:findre` first, which read as a third word rather
than as `find` plus a marker.

**Smart case**, ripgrep's rule: insensitive until you type a capital.

## Scoping to a directory

```
:find src/ needle
:replace docs/ /old/new/
```

**A first word ending in `/` that names a directory under the root is a
scope**, and the search walks only it. Both conditions, deliberately: the
trailing slash is you saying "directory", and the existence check is what keeps
a pattern like `foo/ bar` searchable — a word that names no directory stays in
the pattern, and the status line echoes the pattern it searched, so a typo'd
scope reads back as the literal it became. The one thing this rule takes is a
literal pattern whose first word names a real directory; `:find~` with the
slash escaped (`src\/ pat`) can still say it.

Matches are reported relative to the project root, `src/` prefix and all — the
scope narrows the walk, not the names. Enter still opens the right file, and
two searches with different scopes read the same way.

## One row per line

The searcher reports a matching *line*, and that is what a row is. A hundred
matches in six files reads as six files, each with its lines under it — which
is what the file headings are for, and why a row does not repeat the path down
the left margin.

Two consequences, both deliberate:

- **A line with two matches is one row.** The column recorded is the first
  match, which is where Enter puts the cursor.
- **A replace rewrites every occurrence on the line**, not the first. A row
  that says `needle and needle` and then replaced half of itself would have
  lied about what it was offering. That is why [`Results`] carries the whole
  `Query`: rewriting re-finds within the line using **the same matcher that
  reported it**, and a second engine that agrees today is a second engine that
  can disagree tomorrow.

## A pane, not an overlay

A third `Content` beside text and the file tree — which is exactly what
`windows.md` said the next pane kind should be: "a variant and a compiler
error, not a second boolean."

An overlay would have been less code and the wrong shape. The list outlives the
moment you read it: you scroll it, you leave it open beside the file, you come
back to it. And it is worth building once because diagnostics, LSP references
and git-grep all want this same list — each becomes a producer of `Results`
rather than another overlay to design.

The pane **replaces what the focused window was showing**, keeping it as the
alternate, so `q` puts your file back. A split first is how you get both at
once; that is one keystroke, and it saves this command a policy about where
results ought to live.

**Nothing found leaves the pane you were in.** An empty results pane that
displaced your file in order to say "no" is a worse answer than a line of text
saying the same thing.

## Keys

```
j k  ↑ ↓     a row            Enter  o   open it
gg  G        either end       q  Esc      put back what the pane displaced
C-d  C-u     ten rows         C-w …       windows, as everywhere
x            drop the row — a hit, or a file with everything under it
a            apply this rewrite            (armed pane only)
A            apply every rewrite left      (armed pane only)
```

Deliberately small. Everything a results pane cannot do — operators, insert
mode — keeps meaning what it means everywhere else rather than being quietly
swallowed. `Ctrl-W` works because moving between windows is not something a
pane gets to opt out of.

Enter on a **heading** opens the top of that file. It is a row you can press
Enter on, you wanted that file, and the top of it is a perfectly good answer.
Enter on a **hit** lands on the match's column, not on the start of the line —
the column is the whole reason the row was worth showing.

**`x` drops a row from the list** — a hit, or a heading with every hit under
it. It edits nothing; it narrows what the list is offering, which is what
makes "replace everywhere except this directory" a few keystrokes on the pane
rather than a syntax. The count in the heading follows.

## Replacing

```
:replace /{pattern}/{replacement}/
:replace //{replacement}/
```

**The argument is delimited, always** — the same delimiter rule as `:s`:
whatever non-alphanumeric character follows the space, `\` escaping it, the
closing one optional. There is no bare-word spelling, because a replacement is
text and text can start with anything: `:replace /usr/local` would have had to
guess whether it was a replacement or a search, and a command that guesses
about a project-wide rewrite is the wrong command. An argument that does not
start with a delimiter is an error showing the shape.

**An empty pattern is what the pane is showing** — the same convention as
`:s//new/`. With a results pane focused (a `:find`, or the references pane
`gr` builds), `:replace //new/` previews rewriting exactly the rows on it;
that is how a replace inherits a search you have already narrowed with `x`,
and it is what makes `gr` then `:replace //new/` a rename. With a pattern, the
search runs first — `:replace /old/new/` is `:find old` and the preview in one
line, no prior `:find` needed.

### Preview first

`:replace` never rewrites on Enter. It **arms** the results pane: the title
becomes `replace: old → new`, and every hit row shows its line *as it will
read*, the inserted text highlighted. Then the pane's own keys decide:

- `a` applies the selected row — or, on a heading, that whole file — and the
  row gets its ✓.
- `A` applies everything still pending.
- `x` drops a row from the offer, as it always does.
- `Enter` still opens the file at the match, because deciding sometimes needs
  the surrounding code; `:results` or `Ctrl-^` puts the pane back, still
  armed, decisions intact.
- `q` walks away. Nothing was applied that you did not press `a` or `A` for.

This is the multi-cursor shape — see everything, act per-site or en masse —
applied to the project. A y/n/a/q interrogation was the alternative and is the
reason `:%s///g` is what people actually type; a list you prune and then
commit is the same review without the hostage-taking.

### Into buffers, never straight to disk

Every applied rewrite goes into a buffer: the file is opened (or reused if
already open — the only answer that cannot lose unsaved work), edited as one
undo step per apply, and left modified. `:wa` commits the lot; `u` in any one
of them takes that file back. The unwritten buffers and the undo history are
the second review, after the preview was the first.

**A line that has moved on since the search is skipped and counted.** The line
is compared against what the search recorded before anything is written; the
file may have changed underneath, possibly because you edited it yourself, and
rewriting a line that no longer says what it said is how a bulk replace eats a
repository. One row per line is what makes that check possible at all. The
count is always reported — a skipped line is one you need to look at again,
not one to quietly leave.

An empty replacement is allowed: deleting every match is a thing people mean.

### `:replace~`

The same command with the pattern read as a regular expression, and the
replacement read with it: `$1` is the first capture group, `$name` a named
one, `$$` a literal dollar. `:replace~ /fn (\w+)/fn new_$1/` is a rename with
the interesting half kept. Under `:replace` — a literal pattern — the
replacement is literal too, dollars and all: groups you could not have written
do not deserve syntax you have to escape.

The pattern names the language, so `:replace~` with an empty pattern inherits
the pane's query as it was — a literal query stays literal. The `~` had
nothing to attach to.

## Going back

`:find needle`, Enter on the one interesting row, read the code — and the pane
is gone, displaced by the file it opened. Two ways back, and they are the two
that already exist:

- **`Ctrl-^`** — the pane went to the window's alternate slot when Enter
  displaced it, exactly as a parked tree does, and the swap back treats it the
  same way. One keystroke while the trail is fresh.
- **`:results`** — the session keeps the last results list that left a
  window, selection, decisions and all, and `:results` shows it in the focused
  window (which becomes the alternate, so `Ctrl-^` flips between file and
  list from then on). This is the one that still works after the alternate
  slot has moved on to other things.

Nothing here re-runs the search: what comes back is the list as you left it,
prunes and ✓s included. A search you want fresh is a search you type again.

## Limits, said out loud

`LIMIT` is 10 000 matches, and hitting it is reported. A list that stops
somewhere and does not say so reads as an answer. Unreadable files are counted
rather than fatal — one bad file in a tree of ten thousand is a fact to
mention, not a reason to have no results. Binary files are skipped, ripgrep's
own default: one match inside one would print a screen of control characters.

## Tests

- Literal, regex, smart case, and the `.gitignore` walk, in `find_in_files.rs`
  against a real temporary tree.
- The column is in characters, not bytes — a match after an `é` is otherwise in
  the wrong place.
- `a(` is a valid literal search and an invalid regex, and says so only in the
  second case.
- A pattern that can match nothing (`x*`) terminates and eats no text.
- Rewriting a line reports the spans it inserted, in characters, so the
  preview can highlight them; `$1` interpolates under a regex query and stays
  two characters under a literal one.
- Grouping, selection clamping, scrolling, `x` on a hit and on a heading, and
  the armed state, in `results.rs`.
- End to end: `:find` fills a pane, Enter opens at the match, a heading opens
  the top, `q` puts the file back.
- `:find src/ needle` searches only src/ and reports paths with their prefix;
  a first word naming no directory stays in the pattern.
- `:replace /old/new/` arms a preview; `a` applies one row into its buffer,
  `A` the rest, and the disk is untouched until `:wa`.
- `:replace //new/` inherits the pane, prunes included; over a references
  pane it is a rename.
- a line that changed since the search is skipped and counted.
- `:replace old` — no delimiter — is an error showing the shape.
- `:replace~` interpolates `$1`; `:replace` leaves `$1` alone.
- `Ctrl-^` after Enter swaps the pane back; `:results` brings it back later,
  as it was.
