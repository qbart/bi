# `gq` — reflow

A comment grows a sentence in the middle and the lines are ragged. `gqip`
rewraps the paragraph to `textwidth`, comment leaders and indentation kept,
words moved between lines until each is as full as it can be. Vim's format
operator, for the one thing it is actually used for: prose in code.

## Status

**Built.** `gq{motion}`, `gqq`/`gqgq`, visual `gq`, the `textwidth` option,
indent and comment-leader preservation.

## The keys

```
gq{motion}   gqip  gqj      reflow the lines the motion touches
gqq          gqgq           reflow the cursor's line
gq           (visual)       reflow the selected rows
```

`gq` is an operator in the full sense, the same sense `>` is: it takes a
motion or a text object, it doubles, `.` repeats it. `gqq` is the doubled
form and `gqgq` is the same key with vim's other spelling; both cover the
cursor's line — `gqip` is how you say "the paragraph", exactly as it is for
`d` and `>`.

**Always linewise**, like `>`: there is no reflowing half a line, so the
motion's range widens to the rows it touches.

**Captures nothing.** No register is involved; the `sink` is ignored the way
`>`'s is.

**The cursor lands on the first non-blank of the last line produced**, which
is vim's habit of leaving you at the end of what was formatted, ready to
continue below it.

## `textwidth`

One option, in columns.

```toml
[options]
textwidth = 80
```

The default is 80 rather than vim's 0 — vim's 0 means "no automatic wrapping
while typing", but bi does not wrap while typing at all, so 0 would leave
`gq` with nothing to aim at. The option means one thing here: the width `gq`
folds to. `:set textwidth 100` for the projects that decided otherwise.
Anything below 1 is refused.

Width is display columns, not characters — a tab counts what `tab_width`
says, the same rule every column in bi follows.

## What a paragraph is

Within the rows the motion names, paragraphs are runs of non-blank lines;
blank (or whitespace-only) lines separate them and pass through untouched.
Each paragraph reflows on its own, so `gqip` and `=`-style whole-file sweeps
do not glue paragraphs together.

## The prefix

The first line of the paragraph names it: its leading whitespace, then a
comment leader if one is there — one of

```
///  //!  //  #  --  ;  *  >
```

— then one space. Every following line of the paragraph must carry the same
leader (its own indentation may differ; the first line's wins) for the leader
to count; a paragraph where line two starts with something else keeps only
the whitespace as prefix. The reflowed paragraph is the words of every line,
prefix stripped, repacked greedily: each output line is the prefix plus as
many words as fit in `textwidth` — always at least one, so a word longer
than the width gets a line to itself and is never split.

`#` covers shell, Python, TOML and Ruby; `--` Lua, SQL and Haskell; `;` Lisp
and ini files; `*` the interior of a `/* */` block; `>` a Markdown quote.
The list is a table in `reflow.rs`, not configuration — adding a language is
adding a string.

## What it does not do

- **No hanging indents.** A Markdown list item wraps flush, not under its
  text. Vim's `formatoptions` grammar is a spec of its own; a `gq` that
  handles comments is the 90% that was on hold.
- **No sentence rules,** no two-spaces-after-a-period preservation: a word is
  a run of non-whitespace, joined by single spaces.
- **No format-while-typing.** `textwidth` does not make insert mode wrap.

## Where it lives

- `src/reflow.rs` — pure: lines in, lines out, a width and a tab width. All
  of the paragraph, prefix and packing rules above, testable without a rope.
- `Buffer::reflow_rows` applies it to a row range through the same edit
  plumbing `indent_rows` uses.
- `Operator::Reflow` in `motion.rs`; `input.rs` reads `gq` (and doubles it);
  the editor's arm sits beside `Indent`'s, which it is shaped after.

## Tests

In `reflow.rs`, no buffer involved:

- a long line breaks at the width; short lines merge; the join is one space.
- a `// ` paragraph keeps `// ` on every produced line, width counted with
  the prefix.
- indentation without a leader is kept.
- a word wider than the width stands alone and is not split.
- two paragraphs stay two; the blank line between them survives.
- a leader on line one that line two lacks demotes the prefix to whitespace.
- tabs in the indent count as `tab_width` columns.

In `editor.rs`:

- `gqq` wraps the cursor's line to `textwidth`.
- `gqip` wraps the paragraph and leaves the one below alone.
- visual `gq` wraps the selected rows and returns to normal mode.
- the cursor lands on the last produced line's first non-blank.
- one undo step.
- `.` repeats it.
- `:set textwidth 40` changes where the next `gq` folds.
