# `:uniq`, `:dedup`, `:reverse`, `:align`

Whole rows, rearranged. The family `:sort` started, and the same rules: the
file when nothing narrows it, a range or the selected rows otherwise, one undo
step, a report.

```
:uniq             runs of identical adjacent lines become one
:dedup            every repeat dropped, wherever it is; the first stays
:reverse          last line first
:align =          spaces before the first `=` on each line, until they share a column
:align! :         spaces after it instead, so what follows lines up
```

## Status

**Built.**

## Scope

**No range is the whole file**, which is what `:sort` does and for the same
reason. A selection or an address narrows it; there is no reversing half a
line, so a scope that is not whole rows is widened to the rows it touches and
says so — the same `whole lines` note `:m`, `:retab` and `:sort` give a
rectangle. See [ranges.md](ranges.md).

## `:uniq` and `:dedup`

The difference is *adjacent* against *anywhere*, the same split as `uniq(1)`
against `sort -u`. `:uniq` collapses a run of identical lines into one and
leaves a repeat further down alone — repeated log lines, three blank lines in
a row. `:dedup` drops every line that already appeared earlier in the range,
wherever it was, keeping the first in place — an import list, a wordlist you
cannot sort. `:sort u` is `:dedup` for a range you *can* sort.

Both report `3 lines dropped`, and say `nothing to drop` when there was
nothing to drop.

## `:reverse`

Last line first. `2 lines reversed`; one line is `nothing to reverse`.

## `:align`

`:align seq` finds the first `seq` on each line and inserts spaces before it
until every one sits in the same column. `:align! seq` inserts them *after*
it, so that what follows lines up — `:align =` for assignments, `:align! :`
for a key-value block:

```
a = 1        a   = 1        key: v        key:     v
bbb = 2      bbb = 2        longkey: v    longkey: v
```

- **It only ever adds.** A line whose `seq` already sits past the others is
  the column everyone else moves to; existing spaces are kept, not
  normalised. That is what makes the command idempotent — `:align =` twice
  is `already aligned on `=``.
- **Columns are screen columns**, so a tab counts `tab_width`, and a
  double-width character two. The padding is spaces regardless.
- Lines without `seq` are left alone and not counted. No line with it is
  `no `=` here`, and nothing changes.
- The argument is trimmed, so `:align =` and `:align  = ` are the same
  command; there is no aligning on whitespace.
- Without an argument: `align on what? `:align =``.

## Where it lives

`src/lines.rs` holds the four as one enum — lines in, lines and a report out,
no buffer in sight:

```rust
pub enum LineOp { Uniq, Dedup, Reverse, Align { seq: String, after: bool } }

impl LineOp {
    pub fn parse(name: &str, arg: &str, force: bool) -> Option<Result<Self, String>>;
    pub fn apply(&self, lines: Vec<String>, tab_width: usize) -> Result<(Vec<String>, String), String>;
}
```

The doing is `View::rewrite_whole_rows`, which `:sort` was refactored onto:
resolve the scope, widen to rows, hand them to the op, write back what
changed as one `replace_range`, cursor on the first row. Rows that come back
as they went in are no edit and no undo entry, for the reason `:sort` gives.

## Tests

In `lines.rs`: each op on its own; `:uniq` against `:dedup` on the same
input; `:align` before and after, only adding, measuring tabs on screen, and
saying so when nothing matches.

In `editor.rs`: `:uniq` then `:dedup` on one file; `:2,3reverse` touches
those rows and undoes in one step; `:align =` over the file, `:'v align! :`
over selected rows, and the two refusals.
