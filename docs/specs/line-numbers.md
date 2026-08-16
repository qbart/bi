# Line numbers

The gutter numbers every line, and that is the only thing it has ever done.
Three things people actually want from it — nothing, relative numbers, and a
sparse ruler — are one option apart.

## Status

**Built.**

## The option

```
:set lines 0     no gutter at all
:set lines -1    relative to the cursor line
:set lines 1     every line numbered — the default
:set lines 5     every fifth line numbered, the rest blank
:set lines       report the current value
```

`:set lines=5` works too, because that is the spelling vim's `:set` uses and
the fingers do not ask first.

**This is not vim's `lines`.** There, `lines` is the height of the terminal.
bee has no reason to expose that — the terminal already knows — and `lines` is
the word that means "the numbers down the side" to everyone who has not read
`:help options`. Written down here so the divergence is a decision rather than
an accident.

## What each row shows

```rust
pub enum LineNumbers {
    Off,
    Relative,
    /// Every `n`th line. `Every(1)` is the plain numbering, and the default.
    Every(usize),
}
```

One function decides, so the renderer holds no rules of its own:

```rust
/// What to print beside `row`. `None` is a blank gutter cell.
pub fn label_for(&self, row: usize, cursor_row: usize) -> Option<usize>
```

- **`Off`** — there is no gutter, so nothing calls it.
- **`Relative`** — the distance from the cursor row, in either direction.
- **`Every(n)`** — the line's own number when it is a multiple of `n`, and
  nothing otherwise.

**The cursor's row always shows its own absolute number**, in both modes. It is
the one number a relative gutter cannot tell you and the one you need to type
`:42`, and vim's `number` + `relativenumber` pair exists precisely because
everybody wants both at once. In `Every(n)` it means the row you are on is
never the blank one.

## Width

The gutter is as wide as the largest line number in the buffer, plus a space —
what it is today — and stays that width in every mode except `Off`, where it is
zero.

Sizing `Relative` to its own largest label instead would make the gutter
narrower, and would make it *change width as the cursor moves*, which moves
every line of the file sideways while you scroll. A stable gutter is worth more
than three columns.

`Off` genuinely removes the column rather than blanking it: everything the
renderer draws is already offset by `gutter`, so zero is a value that path
already handles.

## Where it lives

`Editor` owns the setting, `render` asks. The rules are in the library so a
second frontend gets them for free, and because "what does row 12 show" is not
a question about terminals.

`:set` arrives with it, and starts with exactly one option. A real options table
wants the config layer that `editor.rs` has been waiting for; until then, one
match arm and an honest error for anything else is the whole of it.
