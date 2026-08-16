# Line numbers

The gutter numbers every line, and that is the only thing it has ever done.
Three things people actually want from it — nothing, relative numbers, and a
sparse ruler — are one option apart.

## Status

**Built.**

## The option

```
:set number 0     no gutter at all
:set number -1    relative to the cursor line
:set number 1     every line numbered — the default
:set number 5     every fifth line numbered, the rest blank
:set number       report the current value
```

`:set number=5` works too, because that is the spelling vim's `:set` uses and
the fingers do not ask first.

**It takes a value where vim's `number` is a boolean.** `:set nu` and
`:set rnu` are two options in vim because a boolean cannot say "every fifth";
one option that takes a number says all three, and off is just `0`. The cost is
that `:set nu` does not work — bee has no boolean form and no `no` prefix — and
that bare `:set number` reports rather than turning numbering on. Written down
so the divergence is a decision rather than an accident.

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

`Session` owns the setting, `render` asks. The rules are in the library so a
second frontend gets them for free, and because "what does row 12 show" is not
a question about terminals.

**It is session-wide, where vim scopes `'number'` per window.** The second
divergence from vim on this page, and a decision rather than a step not yet
taken. The gutter is a reading preference — how you like files to look — not a
fact about one particular view of one, so scoping it per window would mean the
same file reads differently depending on which pane you opened it in, and that
every new split inherits its numbering from whichever window it was born from.
Both answer a question nobody asked.

The cost is that there is no way to number one pane and leave its neighbour
bare. See [windows.md](windows.md).

`:set` arrives with it, and starts with exactly one option. A real options table
wants the config layer that `editor.rs` has been waiting for; until then, one
match arm and an honest error for anything else is the whole of it. Note that
the config layer does not change the scope decision above — a per-window
override is something to refuse, not something to get around to.
