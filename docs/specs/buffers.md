# Switching buffers

`:ls` listed the open buffers in the order they were opened, which is the order
you stopped caring about them in. The one you want is nearly always the one you
were just in, and the second-nearly-always is the one before that.

## Status

**Built.** The list is the same picker `:ls` always opened; what changed is its
order, where it starts, and how it filters.

## Most recently shown, first

The list is ordered by when each buffer was last *shown*, not focused. A buffer
you put in a split beside you is one you are working with, and the list is
about what you are working with rather than about where the cursor happens to
be at this instant.

**It opens on the second row** — the buffer you were in before this one. So
`Ctrl-Tab` then `Enter` is a switch back, and doing it twice returns you, which
makes the switcher a toggle for the price of the key you already pressed. It is
the same thing `Ctrl-^` does, reached the way you reach everything else.

Typing leaves that row behind, because a default chosen for the whole list says
nothing about a filtered one; backspacing to an empty query returns to it. A
row you moved to yourself is not a default and is clamped like any other, which
is what every other picker does and what the register list needs.

## Filtering is a subsequence

A buffer name is a path, so it matches the way the file picker matches: `sfr`
finds `src/find/render.rs`. Over prose that rule matches everything, which is
why the register ring still wants whole terms — see `docs/specs/files.md`.

## No preview

A list and nothing else, the way the file picker is. The preview pane exists to show a
register entry that is longer than its row; a buffer is a file you know — you
are switching *back* to it — so its first line tells you nothing you were
missing and takes a third of the overlay to say it. Rows are what this picker
has to spend space on.

That leaves the register ring as the only kind that previews.

## The key

`Ctrl-Tab`, which is what was asked for, with one caveat worth stating: `Tab`
and `Ctrl-I` are the same byte in a terminal, and a terminal that does not
implement the kitty keyboard protocol cannot tell `Ctrl-Tab` from `Tab`.

That costs nothing here. Where the modifier arrives, `Ctrl-Tab` opens the
switcher; where it does not, the key that arrives is a plain `Tab`, which is
buffer-next, which is exactly what it did before. Nothing is taken away and no
key had to be given up — which is the reason the *window* picker went to
`Ctrl-W f` instead of `Tab`: that one would have had to take `Ctrl-I` with it.

`:ls` opens the same list, always, in every terminal.

**`gb` is the default that always arrives.** `Ctrl-Tab` is the key this was
asked for and the one to use where it works, but "where it works" is a
property of the terminal, and a switcher you cannot reach on half of them is
not a default. `gb` sits under the `g` prefix beside `ga`
(`docs/specs/alternate.md`) and `gf` (`docs/specs/files.md`): `b` for the
buffer list, `f` for the file picker. It was `gf` before the file picker
claimed the letter — *file* is what `f` means, and the switcher is buffers.

## Tests

- Three buffers opened in order list newest-first.
- Opening and accepting is a switch to the previous buffer; twice is a toggle.
- Typing leaves the default row and matches a subsequence.
- The overlay is a list: no preview pane, the same as the file picker.
- `gb` opens it, and `ga` still reaches the alternate file.
