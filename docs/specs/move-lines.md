# Moving lines

Reordering a file means yanking lines, moving, and pasting them back — three
commands and a register spent, for what is one thought. `:m` is that thought,
and `Shift-Up` / `Shift-Down` are the same thing without the colon.

Two halves, and they answer different questions. `:m` is vim's `:move`, exactly
— you know the line you want to be after. The arrows are bi's own — you know
how far, and counting rows to turn that into a line number is the work the
command was supposed to save.

## Status

**Built.**

Three corrections below, marked **Corrected**: how many edits a move takes,
what a file with no final newline needs — two cases, of which one was written
down — and what the argument to `:m` means, which is now vim's address in every
form rather than the distance this spec first chose.

## What `m` costs

`m` is vim's mark key, and README's *Known gaps* lists marks (`m{a}`,
`` `{a} ``) as missing. Spending `m` on an ex command does not take it: `:m` is
a command line, and `m` in normal mode stays free for the mark that may yet
land there. That is the reason this is an ex command rather than a key — the
keys are the arrows.

## The command

```
:m 12       after line 12
:m 0        after line 0 — the top
:m $        after the last line
:m +3       after `.+3`
:m -2       after `.-2`, which is one row up
:m .+1      the same address written out
:m+1        the same again, with the space vim does not require
```

**Two spellings that had to be added, and the reason is the same both times.**
`.` is the cursor's line, so `+3` already means `.+3` — but `:m .+1` and
`:m .-2` are what a decade of vimrcs actually say, and so is `:m+1` with
nothing between the command and its address. Both used to miss: one was an
address that did not parse, the other a command called `m+1`. A command that
is vim in one spelling and an error message in the other is worse than either,
and the point of matching vim here is muscle memory.

`m` is the only command that lets its argument touch its name, and it can be
because it is the only one whose argument starts with a character no command
name contains. `:mark` still reads as `mark`.

**A typed range — `:2,3m 4` — is not supported**, here or anywhere else in
bi's `:` line: there is no range parser, and one command wanting one is not a
reason to grow it. The range comes from the selection instead, which is the
same block said with the keys you already have.

Over a visual selection, the whole block moves and stays selected, so a second
`:m` — or a second `Shift-Down` — carries on from where the first left off.

**Every form is an address**, which is to say a line to land after, and none of
them is a distance. `.` is the cursor's line, so `+3` is `.+3`; `:m -1` names
the line above and therefore moves nothing at all, and `-2` is how you go up
one. That reads like an off-by-one and is not one: it falls out of "after",
and it is why every vimrc in the world binds `:m .+1` and `:m .-2`.

Three consequences worth stating, because each is a thing someone will file as
a bug:

**It is direction-dependent.** The address names a line in the buffer as it
stands, so a block arriving from above leaves a hole the address falls through,
while one arriving from below finds everything above the address untouched.
From line 2, `:m 4` becomes line 4; from line 5, `:m 2` becomes line 3.

**An address off either end is refused**, not clamped, and says so. A typed line
number is a claim about a line that either exists or does not. The arrow keys
clamp, because they name no line — that difference is the whole distinction
between the two halves.

**An address inside the block moves nothing.** With lines 2 and 3 selected,
`:m 2` and `:m 3` are no-ops, which falls out of the arithmetic rather than
needing a rule.

**Corrected, twice, and this is the second.** `:m` first read `+N` as N rows
down and a bare `N` as "become line N". The bare number went first, being a
divergence nobody would notice until it bit them; the signed forms followed,
because half a divergence is worse than either whole — the point of matching
vim is that `:m .-2` does what a decade of muscle memory says, and that is not
true of a command that is vim in one form and not in another.

What survives is the distance model, moved to where it belongs: the arrows.

All of it is measured against vim 9.0 rather than remembered, by running both
editors over the same file and diffing the results: every cursor line against
every absolute address in a five-line buffer, every two-line block against a
spread of them, and every cursor line against the signed forms and the bare
`+` and `-`. 115 combinations, no disagreements. `scripts/` is where that
harness belongs if it is ever wanted again — `vim_differential.py` already does
the same job for motions.

**A move that would run off either end clamps** rather than refusing. `:m +99`
on the third-from-last line means "to the bottom", which is what someone typing
a big number wants; refusing it would be correct and useless.

## The keys

```
Shift-Down    move it down one
Shift-Up      move it up one
```

With a count, `3 Shift-Down` moves three, and running off the end simply stops.
In visual mode they move the selection and keep it, which is what makes nudging
a block a matter of holding a key rather than counting rows first.

These are the distance half, and they are not vim — vim has no such key, and
the mappings everyone writes for it are `:m .+1` and `:m .-2` wrapped in `gv=gv`
to get the selection back. `Shift-Down` is that, without the wrapping. It is
also why `:m` can be vim-exact without anyone having to count rows: the
question "how far" now has a key of its own.

`Key` has carried `shift` since it was written — "`alt` and `shift` are carried
even though the keymap reads neither today", says `key.rs`, and this is the
today when one of them starts being read. Terminals differ about whether they
send a modifier with an arrow key; the ones that do not simply get the plain
arrow, which still moves the cursor, and `:m` still works everywhere.

## What happens to the text

Two edits — the lift and the drop — inside one command, so the whole move is a
single undo step and both land in the log `settle` drains. Another window on
the same buffer follows the lines rather than being scattered by them, which is
the property every other edit already has and the reason this one goes through
`edit_raw` like anything else.

**Corrected.** This first said "one edit each way", which is not a thing a move
can be: taking the lines out and putting them back are two edits however it is
written. What matters is that they are one *command*, which is what the undo
group is keyed on.

The cursor rides with the lines: it lands on the first row that moved, in the
column it was in. Anything else makes a repeated `Shift-Down` walk away from
the line it is supposed to be pushing.

```rust
impl Buffer {
    /// Moves rows `first..=last` so the block starts at `to`.
    pub fn move_lines(
        &mut self,
        first: usize,
        last: usize,
        to: usize,
        col: usize,
    ) -> Option<Cursor>;
}
```

`col` is there because the cursor rides along, and the buffer is the only thing
that can put it back on a column once the rows have moved.

Two edge cases the rope makes real, and both are about the last line:

**A file whose last line has no terminator**, in two directions rather than one.
Dropping a block *past* such a line would weld it to the line above, so the
newline moves to the front of what is inserted and its trailing one is dropped.

**Corrected.** That was the only half written down here, and it is the half
that does not bite. Lifting the final line *out* is the other: it has no
trailing newline to take, so a naive lift invents one, and the file that had no
terminator suddenly has one. It takes the newline that ended the line *above*
instead — cutting from one character earlier — and the file keeps whatever it
had. A test moving the last line of an unterminated file up is what found it;
the first implementation passed every other case.

Both branches say the same thing: a move reorders lines and must not decide
whether the file ends in a newline.

**Moving the block onto itself is not an edit.** `to == first` returns `None`
and touches nothing, so `Shift-Up` on the first line neither errors nor puts a
no-op on the undo stack.

## Tests

- Down one, up one, in the middle of a file, cursor riding along.
- A three-line block moved as one, still selected afterwards.
- `:m 0` and `:m $` from the middle.
- `:m .+1`, `:m+1`, `:move.+1` and `:m .-2` all reach the same address the
  spaced signed forms do; `:m .` and `:m .-1` move nothing.
- `:mark` is still an unknown command and `:move` alone still asks where.
- `:m +99` clamps to the bottom rather than refusing.
- `Shift-Up` on the first line and `Shift-Down` on the last do nothing at all —
  no edit, nothing to undo.
- A file with no trailing newline, moving the last line up *and* another line
  to the end — both directions, because that is where the rope arithmetic is
  and only one of them was foreseen.
- Undo puts the file back in one step.
- A second window on the same buffer follows the lines, which is the property
  `settle` already gives every other edit.
