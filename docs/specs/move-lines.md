# Moving lines

Reordering a file means yanking lines, moving, and pasting them back — three
commands and a register spent, for what is one thought. `:m` is that thought,
and `Shift-Up` / `Shift-Down` are the same thing without the colon.

## Status

**Built.**

Three corrections below, marked **Corrected**: how many edits a move takes,
what a file with no final newline needs — two cases, of which one was written
down — and what a bare number means, which is now vim's address rather than
this spec's first answer.

## What `m` costs

`m` is vim's mark key, and README's *Known gaps* lists marks (`m{a}`,
`` `{a} ``) as missing. Spending `m` on an ex command does not take it: `:m` is
a command line, and `m` in normal mode stays free for the mark that may yet
land there. That is the reason this is an ex command rather than a key — the
keys are the arrows.

## The command

```
:m +3       down three rows
:m -2       up two rows
:m 0        after line 0 — the top
:m $        after the last line
:m 12       after line 12
```

Over a visual selection, the whole block moves and stays selected, so a second
`:m` — or a second `Shift-Down` — carries on from where the first left off.

The argument is read two ways, and the split is on the sign.

**A bare number is vim's address**, exactly: the lines land *after* line N, and
`0` and `$` are the same addresses they always were. This is why the absolute
form is direction-dependent, and why that is not a bug — the address names a
line in the buffer as it stands, so a block arriving from above leaves a hole
that the address falls through, and one arriving from below finds everything
above the address untouched. From line 2, `:m 4` becomes line 4; from line 5,
`:m 2` becomes line 3. Both measured against vim 9.0, along with every other
combination in a five-line buffer and every two-line block in one.

**Corrected.** A bare number first meant "become line N", which is neither vim
nor obviously better: it reads the same for a block travelling down and differs
only going up, so it was a divergence nobody would notice until it bit them.
The address is the older, better-known answer and costs nothing to match.

**A signed number is a distance.** `+3` is three rows down and `-2` is two rows
up. Vim reads those as addresses too — `.+3` and `.-2` — which is why `:m-2`
there travels one row and `:m-1` travels none. That is a fine primitive and a
poor command: an off-by-one between the number typed and the rows travelled is
a trap that never stops being one, and `Shift-Up` needs a distance behind it
anyway. This half of the divergence is deliberate and stays.

**A move that would run off either end clamps** rather than refusing. `:m +99`
on the third-from-last line means "to the bottom", which is what someone typing
a big number wants; refusing it would be correct and useless.

## The keys

```
Shift-Down    move it down one
Shift-Up      move it up one
```

With a count, `3 Shift-Down` moves three. In visual mode they move the
selection and keep it, which is what makes nudging a block a matter of holding
a key rather than counting rows first.

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
- `:m +99` clamps to the bottom rather than refusing.
- `Shift-Up` on the first line and `Shift-Down` on the last do nothing at all —
  no edit, nothing to undo.
- A file with no trailing newline, moving the last line up *and* another line
  to the end — both directions, because that is where the rope arithmetic is
  and only one of them was foreseen.
- Undo puts the file back in one step.
- A second window on the same buffer follows the lines, which is the property
  `settle` already gives every other edit.
