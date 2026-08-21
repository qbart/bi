# Finding, and replacing, across the project

`/` searches the buffer. `:find` searches the project, and puts what it found
in a pane you can leave open beside the file you are editing.

## Status

**Built.**

```
:find needle        every match under the project root
:findre \bfn \w+    the same, read as a regular expression
:replace pin        rewrite every match the pane is showing
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

**Smart case**, ripgrep's rule: insensitive until you type a capital.

## One row per line

The searcher reports a matching *line*, and that is what a row is. A hundred
matches in six files reads as six files, each with its lines under it — which
is what the file headings are for, and why a row does not repeat the path down
the left margin.

Two consequences, both deliberate:

- **A line with two matches is one row.** The column recorded is the first
  match, which is where Enter puts the cursor.
- **`:replace` rewrites every occurrence on the line**, not the first. A row
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
```

Deliberately small. Everything a results pane cannot do — operators, insert
mode — keeps meaning what it means everywhere else rather than being quietly
swallowed. `Ctrl-W` works because moving between windows is not something a
pane gets to opt out of.

Enter on a **heading** opens the top of that file. It is a row you can press
Enter on, you wanted that file, and the top of it is a perfectly good answer.
Enter on a **hit** lands on the match's column, not on the start of the line —
the column is the whole reason the row was worth showing.

## Replacing

**Into buffers, never straight to disk.** Every file with a match is opened,
edited as one undo step, and left modified. `:wa` commits the lot; `u` in any
one of them takes that file back.

That is the confirmation. There is no prompt because the unwritten buffers and
the undo history *are* the review, and a y/n/a/q walk across a repository is
the reason `:%s///g` is what people actually type.

An already-open buffer is reused rather than re-read, so a replace over a file
you have unsaved edits in edits *those* — the only answer that cannot lose
work.

**A line that has moved on since the search is skipped and counted.** The line
is compared against what the search recorded before anything is written; the
file may have changed underneath, possibly because you edited it yourself, and
rewriting a line that no longer says what it said is how a bulk replace eats a
repository. One row per line is what makes that check possible at all. The
count is always reported — a skipped line is one you need to look at again, not
one to quietly leave.

An empty replacement is allowed: deleting every match is a thing people mean.

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
- Grouping, selection clamping and scrolling, in `results.rs`.
- End to end: `:find` fills a pane, Enter opens at the match, a heading opens
  the top, `q` puts the file back.
- `:replace` rewrites into buffers, leaves the disk alone until `:wa`, takes
  both matches on one line, and reports a line that changed underneath.
