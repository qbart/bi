# Search

`/` and `?`, `n` and `N`, `*` and `#`. Exact matching, not fuzzy — see
*Why not fuzzy* below, because the question comes up and the answer is not
"fuzzy is bad".

## Status

**Built.**

Covers literal matching with smartcase, the status line's echo and match
count, and opt-in highlighting. Regular expressions are deferred. `:s` is not:
it is built, on this same literal matcher, and has its own spec at
[substitute.md](substitute.md) — the pattern language and "replace this across
these lines" turned out to be two questions rather than one.

## The commands

| Key | Does |
|---|---|
| `/pat` `<CR>` | search forward |
| `?pat` `<CR>` | search backward |
| `n` / `N` | repeat the last search / repeat it reversed |
| `*` / `#` | search forward / backward for the word under the cursor |
| `:hls` `:noh` | start / stop highlighting every match |

## Semantics

Four properties, each verified against vim rather than remembered:

- **A search is a motion.** `d/three<CR>` deletes from the cursor to the match.
  This is most of why search is worth having, and it is what forces exact
  matching.
- **Exclusive.** It stops *before* the match: `d/three` on `one two three four`
  leaves `three four`.
- **It lands on the first character** of the match. `/three` then `x` gives
  `one two hree`.
- **It wraps.** From the end of `one two`, `/one` finds the one at the start.
  Vim's `wrapscan`, on by default.

`n` repeats in the direction the search was *typed*, so after `?foo`, `n` keeps
going backward. `N` reverses it.

`*` is anchored to word boundaries: on `foo` / `foobar` / `foo` it skips
`foobar` entirely. `#` is the same backward. Neither takes a typed pattern —
they read the word under the cursor.

**Smartcase.** An all-lowercase pattern matches case-insensitively; any
uppercase character in the pattern makes the whole thing case-sensitive. This
is `ignorecase` + `smartcase`, which is what nearly everyone sets, so it is the
default rather than an option nobody can change yet.

## Where the state lives

```rust
pub struct Search {
    pub pattern: String,
    /// `*` and `#` only match whole words.
    pub whole_word: bool,
    /// The direction it was typed with, which is what `n` repeats.
    pub forward: bool,
}
```

On `Editor`, beside `last_find`, and for the same reason: it has to outlive
`Input::reset()`. `Motion::Search { reverse }` carries no pattern, exactly as
`Motion::RepeatFind` carries no character — `Editor` substitutes the real search
before anything resolves it.

### The operator has to survive the search line

`d/foo<CR>` is the case that shapes this. Typing `/` puts the editor into a
mode where keys are pattern text, and the keymap's `reset()` runs on the way in
— so a pending `d` would be lost.

The operator therefore travels with the mode change rather than staying in the
keymap:

```rust
Action::EnterSearch { forward: bool, operator: Option<(Operator, Sink)>, count: usize }
```

`Editor` holds it until `<CR>`, then applies either the motion or the operator
over it. `Esc` on the search line discards both, changing nothing.

## What a search leaves behind

Not highlighting. A plain `/` in vim does not light the buffer up, and painting
every match is noise you then have to clear. `:hls` turns highlighting on for
the session and `:noh` off again; the renderer's pass is unchanged and still
bounded by the viewport, like every other pass in `render`.

What it leaves is the status line itself. While a search is live the footer
shows the search and nothing else — no name, no mode, no position, exactly as
it looks while the pattern is being typed — and it holds two things:

- **The pattern**, prefixed with the direction being travelled rather than the
  one that was typed — `N` after `/foo` echoes `?foo`, because that is the
  command that would repeat the move you just made.
- **`[3/17]`** on the right: which match the cursor is on, out of how many.
  Off a match it counts what is behind, so `[0/17]` means "in front of the
  first" — vim's rule.

A search is live from `/` until the first key that is not part of one. `n`,
`N` and another `/` keep it; `Esc` on the search line and everything else end
it, and the count ends with it — a remembered pattern is not a live search, and
counting at someone who has moved on is the noise the highlighting was.

The rule lives in `Action::is_search` and is applied once, in `Editor::apply`,
because what ends a search is *anything else* — spelling that out per action is
the version a new action forgets to join.

The count is the one thing here that cannot be answered from the viewport, so
it is the one place that scans the whole buffer. `Editor` caches the match
positions against the pattern and `Buffer::edits()`, a counter every mutation
bumps; the scan therefore runs when the text or the pattern changes rather than
once a frame.

## Why not fuzzy

Because `/` is a motion, and a fuzzy match has no contiguous extent. Matching
`fb` against `foo bar` hits `f` at 0 and `b` at 4, so "the match" is a scattered
set of characters. `d/fb` would have to delete *something* — `foo b` is the only
defensible answer and it is not one a user would predict.

The same ambiguity breaks the rest of the family. `n` needs an order, and fuzzy
implies ranking by score while `n` implies buffer position; those disagree.
`*` reads a word and matches it exactly by nature. `:s` cannot substitute over a
scattered match.

There is also the problem `docs/specs/registers.md` already recorded for the
picker: over prose and code, "these letters appear in order" matches nearly
everything. In a two-thousand-line file a two-character fuzzy pattern matches
thousands of positions.

None of which makes fuzzy *jumping* a bad idea — it is a genuinely different
operation, and `picker.rs` already has the query, matching, selection and
preview machinery a fuzzy line-jump would need. It is simply not `/`.

## Deferred

**Regular expressions.** Literal matching first. `Search` gains a flag and the
matcher gains a backend; nothing above it changes, because a regex match is
still a contiguous range.

**`:s`.** Wants ranges (`:%s`, `:1,5s`) as much as it wants patterns, and
ranges are their own piece of work.

**Search offsets** (`/pat/e`, `/pat/b+2`, `/pat/+1`). The last of those makes
the whole motion *linewise*, so offsets are not cosmetic — they change the
`Kind` a search resolves to. Worth doing, not worth doing first.

**`gn`** — select the next match as a visual range, so `cgn` then `.` is a
rename. Cheap once search and visual both exist, and it composes with the `.`
that already works.

**Incremental search** (`incsearch`) — jumping as you type. Needs the render
pass to see a provisional match that no command has produced yet.

## Verified

`scripts/vim_differential.py` covers `/ ? n N * #`, `d/`, `c/`, `y/`, wrapping,
smartcase both ways, the bare `/` repeat, and `.` after a search-delete. All
match vim.

Two cases are recorded as known divergences and are **not** real differences:
`vim -es -c "normal ..."` aborts the whole key sequence when a search fails or
is cancelled, so a trailing `x` never runs there. Interactive vim, driven
through a pty, agrees with bi in both.

One thing the differential could not have caught, because it only compares
files: entering the search line resets the keymap, so a pending operator would
be lost. `d/foo` silently did nothing until the operator was made to travel
with the mode change.
