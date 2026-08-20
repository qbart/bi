# Ranges on the `:` line

`:m 12` names one line. `:%s/old/new/` names all of them, `:2,5d` names four,
and `:'<,'>s/…` names the ones you had selected. Every ex command that touches
text needs to be told which lines, and vim answers that with one small language
written in front of the command name.

bi has had exactly one command that needed it and answered it privately: `:m`
read its argument with a parser of its own, and the lines it moved came from
the selection because there was no way to say otherwise. `:s` is next, and two
commands with two private answers is the point at which the answer belongs in
one place.

## Status

**Built.** `:m`, the bare address and `:s` use it — `:s` is what it was built
for, and [substitute.md](substitute.md) is where it landed.

## The language

```
%           every line — `1,$`
.           the line the cursor is on
$           the last line
12          line 12, counting from one
'<   '>     the first and last line of the selection
+3   -2     `.+3` and `.-2`, because a bare offset is measured from `.`
$-1         any address, plus or minus a count
2,5         from line 2 to line 5
,5          from `.` to line 5 — an omitted address is `.`
2,          from line 2 to `.`
```

An address is a **base** and a sum of offsets, and the two are separate for the
reason `$-1` exists: "one before the end" is a thing you can name without
knowing how long the file is. Offsets stack (`.+1+1` is `.+2`) because summing
them is a loop and refusing them would be a rule to write down.

A bare `+` or `-` is one, which is what a finger reaching for the key rather
than the number means. Vim reads them the same way.

## What a command does with one

**The command decides**, and the parser hands it over rather than resolving it.
Three cases, and they are the whole policy:

- **A range and no command** — `:12`, `:$`, `:%` — moves the cursor to the
  range's *last* line. `:12` was a special case in the parser before this and
  is now the general rule falling out.
- **A command that takes a range** — `:m` today, `:s` next. It gets an
  `Option<LineRange>`: `None` is "the caller said nothing", which is where the
  command's own default lives. `:m`'s default is the selection, which is what
  makes `Shift-Down` and a bare `:m +1` agree about what they are moving.
- **A command that does not** — `:1,5w`, `:2q` — is an **error that says so**,
  not a range quietly dropped. Vim writes part of a file for the first of
  those; bi does not, and a command that ignores half of what you typed is the
  worse of the two ways to not support something.

## Resolving one

Addresses are parsed without a buffer and resolved against one, which is what
lets the parser be a pure function with its own tests and lets `:m` check its
argument against a rule the range does not share.

**A range's lines must exist.** `:2,99d` in a ten-line file is refused and
says which line was wrong, the same way `:m 99` already did — a typed line
number is a claim about a line that either exists or does not, and doing your
best with it is how you delete the wrong four lines.

**`:m`'s argument may be line 0**, because it is a line to land *after* and
"after line 0" is the top of the file. That is the one asymmetry, and it is why
resolution hands back a number rather than a checked row: the bounds belong to
the command, which knows what it is going to do with it.

**A backwards range is swapped**, silently. Vim asks; vim asks because it is
about to delete something. bi's answer is that `:5,2` and `:2,5` are the same
four lines, that nobody types the first on purpose, and that a prompt is a
worse interruption than the thing it is guarding against.

**`'<` and `'>` are the primary selection**, first and last row. bi does not
prefill them the way vim does when you press `:` in visual mode, and it does
not need to: a command that acts on the selection already sees it. They are
here because someone with the muscle memory will type them, and because `:s`
wants a spelling for "the block I had" that is not "the block I have".

## What is deliberately not here

**Search addresses** — `/pat/`, `?pat?`, `\/`. They are a second search
implementation reached through a syntax nobody types; `/` already exists and
the line it lands on is `.`.

**`;` as a separator.** `:.;+3` sets the cursor to the first address before
resolving the second, which means parsing that mutates the editor. One
character of vim, bought with a rule that every future address has to obey.

**Marks other than `'<` and `'>`.** Marks are not implemented (README's *Known
gaps* still lists them); when they are, `'{a}` is one more `Base` and nothing
else here moves.

## Where it lives

`src/range.rs`, with `Address`, `LineRange`, the parser and the resolution —
its own module for the same reason `case.rs` and `trim.rs` are theirs: it is a
language with its own rules, it is testable without an editor, and `:s` will
import it rather than reach into `editor.rs`.

```rust
pub struct Address { pub base: Base, pub offset: isize }
pub enum Base { Current, Last, Row(usize), SelectionFirst, SelectionLast }
pub struct LineRange { pub first: Address, pub last: Address }

/// Reads a range off the front of a `:` line, and hands back what is left.
pub fn parse(line: &str) -> (Option<LineRange>, &str);

impl Address  { pub fn resolve(&self, lines: usize, at: Where) -> isize; }
impl LineRange { pub fn rows(&self, lines: usize, at: Where) -> Result<(usize, usize), String>; }
```

`Where` is the three line numbers resolution needs — the cursor's, and the
selection's two — passed in rather than reached for, so the parser's module
never learns what a `Buffer` is.

## Tests

- Every form above parses to the address it names, and leaves the rest of the
  line untouched.
- A line that starts with no address parses to no range and is returned whole,
  so every existing command still reads the same.
- `%` is `1,$`; `,5` is `.,5`; `2,` is `2,.`.
- Offsets stack and a bare `+` is one.
- `$-1` is the line before the last, in a file of any length.
- A backwards range comes back swapped.
- A range past the end is refused, and names the line that was wrong.
- `:12` with no command goes to line 12; `:%` goes to the last line.
- `:2,3m 0` moves those two lines whatever the selection was, and `:m 0` with
  no range still moves the selection.
- `:1,5w` is an error naming the command, not a write.
