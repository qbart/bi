# Jump list

`Ctrl-O` goes back to where you were before the last jump; `Ctrl-I` goes
forward again. Across files. The way vim spells it, because that is what the
fingers already reach for.

## Status

**Built.** Decisions are resolved at the end.

## Why

`Ctrl-O` and `Ctrl-I` are bound today to buffer-previous / buffer-next over
the *open order* of buffers (`input.rs`, "bi has no jump list and these are
the keys the fingers reach for"). Two things are wrong with that. The order
is not the one anybody expects — a buffer switcher wants most-recent-first,
which `gb` and `Ctrl-^` already give. And the feature is the wrong one: what
the fingers reach for after `gd`, `G`, `/`, or a picker is *where I just
was*, which no buffer order can answer, because "where" is a position, not a
file.

## What a jump is

A **jump** is a motion that can move far — far enough that you cannot see
where you came from. Vim's list (`:help jump-motions`), which bi adopts
whole:

- `G`, `gg`, `:{n}` — [`Motion::FirstLine`], [`LastLine`], [`Line`]
- `/`, `?`, `n`, `N`, `*`, `#` — [`Motion::Search`], [`Found`]
- `%` — [`Motion::MatchingBracket`]
- `(`, `)`, `{`, `}` — [`Motion::Paragraph`] (and sentences, when bi has them)
- any switch of what a window shows: `gd`/`gr`/`:def`/`:decl`/`:impl`, the
  file and buffer pickers, `:e`, `:b`, `Ctrl-^`, a Results row, a tree entry,
  a debugger frame, `:find` hits

Not jumps: `h j k l w b e 0 ^ $ f t ; ,` and the scroll keys. Small motions
you can see happen; recording them would bury the one jump you want under
fifty you do not.

The property lives on the motion — `Motion::is_jump()` — so it cannot drift
from the `match` that applies it, and a new motion has to decide which side
it is on the day it is written.

## The list

Per **window**, like vim, and for vim's reason: two windows on one buffer are
two trains of thought, and `Ctrl-O` in one must not replay the other's
history. It lives on [`window::Text`] beside `selections` and `scroll` — view
state, exactly what those two are.

```
Jumps {
    entries: Vec<Jump>,    // oldest first, capped at 100
    at: usize,             // == entries.len() when not walking
}
Jump { buffer: BufferId, at: usize /* char offset */ }
```

`Jumps::entries()` hands the list back oldest-first, read-only, for anything
that wants to look without walking — a `:jumps` listing, and the tests.

**Before** a jump, the position being left is pushed. Pushing a position that
equals the entry already on top is a no-op, so `n n n` over one hit records
once. Pushing while walking (`at < len`) truncates the forward half first —
a browser's history, not a ring: once you jump from the middle, "forward" is
the new place, not the old one.

`Ctrl-O` from the *end* of the list first pushes the current position, so
`Ctrl-I` can come back to it; that push is what makes `Ctrl-O Ctrl-I` a
no-op pair. Then it moves `at` back one and goes there. `Ctrl-I` moves `at`
forward one and goes there; at the end it does nothing. Both take a count.

Going there means: show that buffer in this window if it is not the one
shown (through `Editor::show`, which carries the leaving position into the
buffer entry exactly as it does now), then place a single collapsed cursor at
the offset, clamped to the buffer's current length.

Entries whose buffer has been closed are skipped on the walk, and dropped the
next time the list is touched. Entries are **char offsets**, and they must
follow the text: the drain in `Editor::settle` that remaps every unfocused
window's selections through each edit remaps these too, in the same pass —
otherwise a jump recorded before a paragraph was deleted lands in the middle
of the wrong function. The mapping is `Edit::map`, already the rule for
selections and diagnostics.

## The keys

- `Ctrl-O` — back. `Ctrl-I` — forward.
- `Tab` — forward, because `Tab` **is** `Ctrl-I` in every terminal; they
  cannot be told apart without the kitty protocol and vim treats them as one
  key. Buffer-next loses `Tab`, and keeps `:bn`, `gb`, `Ctrl-^` and `Ctrl-Tab`
  where the terminal sends one (`docs/specs/buffers.md`).
- `''` and ``` `` ``` — back to the position before the latest jump, without
  walking the list: the same as `Ctrl-O` once, but they also record the place
  you left, so `''` `''` toggles. Cheap once the list exists; the natural home
  of `'` and `` ` `` when marks arrive.

`[keys.normal]` rebinds all of them: `jump_back` (`Ctrl-O`), `jump_forward`
(`Ctrl-I`, which is what `Tab` sends), `jump_last` (`''`). These are three new
rows — buffer-next never had a name of its own; `Ctrl-I`/`Ctrl-O`/`Tab` were
hardcoded and unrebindable before this. Buffer-next keeps `:bn`, `gb`,
`Ctrl-^` and `Ctrl-Tab` (`docs/specs/buffers.md`).

## Where it hooks in

Three seams, each already the single place its kind of move happens:

1. `Action::Move(m)` in `Editor::apply` — if `m.is_jump()`, push the current
   head before applying. Operators (`d/`, `y%`) do not jump: a motion under
   an operator is a range, not a move.
2. `Editor::show(window, buffer)` — push before switching. Every file/buffer
   change goes through it: pickers, `:e`, `gd`, results, tree, `Ctrl-^`.
3. The in-buffer non-motion jumps — `goto_row` (`:{n}`), `apply_goto` when
   the target is the current buffer, the debugger's `jump_source_window` —
   push before moving. Each is a one-line call at a site that already exists.

`Ctrl-O`/`Ctrl-I` themselves move *without* pushing (the walk is not a
jump), which is what keeps the list stable while you flip through it.

## Tests

- `G` then `Ctrl-O` returns to the line left; `Ctrl-I` goes to the end again.
- Two `Ctrl-O` after two jumps go back twice; a fresh jump from the middle
  drops the forward half.
- `n` three times over one hit records one entry.
- `gd` into another file, `Ctrl-O`, is back in the first file at the call.
- A jump recorded above a deleted block still lands on the same text.
- `Ctrl-O` from the end, then `Ctrl-I`, is where you started.
- `''` twice is a toggle.
- `Tab` and `Ctrl-I` produce the same action; `Tab` no longer changes buffer.
- Two windows on one buffer keep separate lists.
- `h`/`j` record nothing; `d/foo` records nothing.

## Resolved decisions

1. **Cap** — 100 per window, vim's default; oldest dropped first.
2. **Per window, never across.** `Ctrl-W` is for windows.
3. **`''` and ``` `` ``` ship now** — one entry of the same list. When marks
   arrive, `'` and `` ` `` become their prefix and these remain the `'`-`'`
   special case, as in vim.

## Deviations from the design

1. **`:s` and `:g` do not record a jump.** In bi they are range commands that
   leave the cursor where the range ends, not moves.
2. **A delete whose end coincides exactly with a recorded jump remaps that
   entry onto the cursor's own position.** `Ctrl-O` then steps *past* it,
   because `Jumps::back` dedupes an entry equal to the position it is walking
   from — the same rule that makes `n n n` over one hit record once. This is
   intended, not a gap: the entry did not vanish, it collapsed into the
   position it now shares with the cursor.
3. **`'x` and `` `x`` (marks) are not bound.** The `'`/`` ` `` prefix is
   reserved for them, as decision 3 above says; today a stray letter after
   `'` or `` ` `` is swallowed rather than doing anything, because there are
   no marks yet to look up.
4. **A window that goes text → tree → *another* file starts a fresh list.**
   The list lives on [`window::Text`], and a window showing a tree has parked
   its one `Text` in the single `alt` slot; coming back to a different file
   builds a new `Text` and the parked one goes, jump list and all. The common
   tree-toggle-back keeps everything, because that *is* the parked `Text`
   returning. Living on `Text` is the design (§"The list"): a second slot to
   carry a list across a buffer the window never showed would be a
   window-level history by another name.
