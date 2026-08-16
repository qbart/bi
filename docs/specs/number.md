# `number`

What the gutter shows, as one option that takes a value.

## Status

**Built.** This describes current behaviour.

## The option

```
:set number 0     no gutter at all
:set number -1    relative to the cursor line
:set number 1     every line numbered — the default
:set number 5     every fifth line numbered, the rest blank
:set number       report the current value
```

`:set number=5` works too — vim's `:set` spelling, which the fingers type
without asking. A value that is neither `0`, `-1` nor a positive count is
refused with a message rather than guessed at.

## What each row shows

```rust
pub enum LineNumbers {
    Off,
    Relative,
    /// Every `n`th line. `Every(1)` is plain numbering, and the default.
    Every(usize),
}

/// What to print beside `row`. `None` is a blank gutter cell.
pub fn label_for(self, row: usize, cursor_row: usize) -> Option<usize>
```

One function decides, so the renderer holds no rules of its own:

- **`Off`** — there is no gutter, so nothing calls it.
- **`Relative`** — the distance from the cursor row, in either direction.
- **`Every(n)`** — the line's own number when it is a multiple of `n`, blank
  otherwise.

**The cursor's row always shows its own absolute number**, in every mode but
`Off`. It is the one number a relative gutter cannot tell you and the one `:{n}`
needs — vim's `number` + `relativenumber` pair exists because everybody wants
both at once. Under `Every(n)` it means the row you are on is never the blank
one.

## Width

As wide as the largest line number in the buffer, plus a space, and that width
in every mode but `Off`, where it is zero.

Sizing `Relative` to its own largest label would be narrower and would change
width as the cursor moves, sliding every line sideways while you scroll. A
stable gutter is worth three columns.

`Off` removes the column rather than blanking it: everything the renderer draws
is already offset by the gutter, so zero is a width that path already handles.

Each window sizes its own gutter, since it is a fact about the file in that
pane. A 90-line file beside a 9000-line one gets the narrower column.

## Two divergences from vim

Both deliberate, written down so they are decisions rather than accidents.

**It takes a value where vim's `number` is a boolean.** `:set nu` and `:set rnu`
are two options in vim because a boolean cannot say "every fifth"; one option
taking a number says all three, and off is just `0`. The cost: `:set nu` does
not work — there is no boolean form and no `no` prefix — and bare `:set number`
reports instead of turning numbering on.

**It is session-wide where vim scopes `'number'` per window.** The gutter is a
reading preference, not a fact about one view, so one value governs every pane.
Scoping it per window would make the same file read differently depending on
which pane you opened it in, and make every split inherit numbering from
whichever window it was born from. The cost: you cannot number one pane and
leave its neighbour bare. The config language is not an occasion to revisit
this — a per-window override is something to refuse. See
[windows.md](windows.md).

## Where it lives

`Session` owns the setting and `render` asks, so a second frontend gets the
rules for free and "what does row 12 show" stays out of the terminal.

`:set` still has exactly this one option. A real options table wants the config
layer `editor.rs` has been waiting for; until then it is one match arm and an
honest error for anything else.
