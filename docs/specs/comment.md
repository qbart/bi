# Comment toggle

`gcc` comments the line out, or back in. `gc{motion}` does it to the lines a
motion covers, `gc` in visual mode to the selection. The marker is the
language's own — `//`, `#`, `--`, `;` — and bi already knows it.

## Status

**Approved for build.** Decisions are resolved at the end.

## Why

Today the only route is `:%normal I// ` (`docs/specs/global.md`), which knows
nothing about the language and nothing about toggling: run it twice and you
have `// // `. Meanwhile `syntax::line_comment(filetype)` has existed since
the tree-sitter-context feature needed to write `} // if (…) {` in whatever
language it was passing through — and its own doc comment names "a
comment-toggle key" as the obvious second client. This is that client.

## The operator

`gc` is an operator in the full sense, the shape `gq` already has: it takes a
motion (`gcj`, `gcap`, `gc}`), it doubles (`gcc`, and `gcgc` through the `g`
block), it takes a count (`3gcc`), `.` repeats it, and in visual mode it acts
on the selection. It captures nothing — no register — and it is **always
linewise**, whatever its motion says: half a line cannot be commented out
with a line marker. That puts it beside `>`, `gq` and `=` in
`Operator` — the capture-nothing, always-linewise family — and, exactly as
for those three, `gcj` needs no machinery of its own beside what `dj` has.

## The toggle

One decision per range, not per line — commentary's rule, adopted whole:

- If **every** non-blank line in the range already starts (after its indent)
  with the marker, the range is commented → **uncomment all** of them.
- Otherwise → **comment all** of them.

So a block with one stray uncommented line becomes fully commented on the
first `gc` and fully uncommented on the second, which is the only behaviour
under which `gc gc` is a no-op for any range.

**Blank lines are skipped**, both ways: they get no marker, and they do not
count against "every line is commented". A commented-out function with a
blank line in the middle is still one commented block.

**Commenting** inserts `marker + " "` at the range's **minimum indent
column** (measured in display columns, so tabs and spaces agree), so the
markers line up in one column and the code's own shape survives underneath:

```
    if x {            //     if x {
        y();     →    //         y();
    }                 //     }
```

**Uncommenting** removes the marker and *one* following space if there is
one — the space `gc` itself added — from wherever it sits after the indent,
so a hand-written `//x` and a `// x` both come back clean.

The whole range is **one edit and one undo step**, like `>` and `gq`.

## Which marker

`syntax::line_comment(filetype)` is the single source. It returns `None` for
languages with no line-comment form — CSS, JSON, HTML and the markup family
— and `gc` there says so on the status line, `no line comment for <ft>`, and
changes nothing. Lending them `//` would produce something that reads as a
mistake in the file, which is the reason `line_comment` refuses in the first
place. Block-comment forms are not attempted: a `/* */` toggle has its own
edge cases (nesting, a `*/` inside the range) and no language in bi's table
needs it to be *usable*.

A buffer with no filetype is `None` too — the status names the gap.

## Keys

- `gcc` — toggle the current line (`{count}gcc`: that many lines).
- `gc{motion}` — toggle the lines the motion covers; `gcgc` is `gcc`.
- Visual `gc` — toggle the selected lines.

`gc` sits in the `g` block beside `gq`. `[keys.normal]` names it
`comment` (the operator); the doubled form needs no name of its own, exactly
as `indent_right` covers `>>`.

## Where it hooks in

- `motion.rs`: `Operator::Comment`, documented beside `Reflow`/`Reindent`.
- `input.rs`: the `g` block's `q` arm gains a `c` twin; the doubled-form
  match gains `(Operator::Comment, 'c')`; `Comment` joins the capture-nothing
  arms wherever `Reflow` is listed (surround, dot-repeat spelling, names).
- `buffer.rs`: `pub fn comment_rows(&mut self, first, last, marker) ->
  Option<Cursor>` — the toggle over rows, one edit, landing the cursor on
  the first row's first non-blank, the shape `indent_rows` has.
- `editor.rs`: an `Action::Operate { op: Comment, .. }` arm beside
  `Reindent`'s and an `OperateSelection` arm beside `Indent`'s, both reading
  the marker from `syntax::line_comment(self.filetype_of(buffer))`.

## Tests

- `gcc` on `    y();` gives `    // y();`; again gives `    y();` back.
- `3gcc` comments three lines; blank lines inside get no marker.
- A range with one uncommented line among commented ones comments all; the
  next `gc` uncomments all — `gc gc` is a no-op.
- The marker lands at the minimum indent of the range; mixed indents keep
  their shape under one column of markers.
- Uncommenting `//x` and `// x` both give `x`.
- `gcj`, `gcap`, `gcgc`, and visual `gc` all reach the same toggle; `.`
  repeats `gcc`.
- Python uses `#`, Lua `--`, an INI file `;`; CSS and JSON refuse with a
  status and change nothing; a buffer with no filetype refuses.
- One `u` restores the whole range.

## Resolved decisions

1. **All-or-nothing per range** (commentary), blank lines skipped and neutral.
2. **Marker at the minimum indent column**, one space after it; uncomment
   strips the marker and one space.
3. **Line comments only**; no marker → a status, no change. Block-comment
   forms are out of scope.
4. **No config override for the marker in v1.** `line_comment`'s table covers
   every language bi highlights; a `[filetype.<name>] comment = "…"` key
   waits for the first language that needs one.
