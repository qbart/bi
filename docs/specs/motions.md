# Motions, operators and text objects

bee's keymap covers twelve motions and three operators. Vim has roughly forty
and twelve. This file records the gap, what closes it, and in what order.

## Status

| Step | Scope | Needs |
|---|---|---|
| **1** ✅ | `:e` `:e!` `:e <path>`; `D C s S X r{char} ~ J` | nothing new |
| **2** ✅ | `f F t T ; ,` and text objects `iw aw i( a" …` | a pending-argument state |
| 3 | `e ge W B E`, real `^`, `g_ + - _`, `%` | nothing new |
| **4** ✅ | `.` — repeat last change | a record of the last change |
| **5** ✅ | `/ ? n N * #` search | see `search.md` |
| **6** ✅ | `Ctrl-E/Y`, `Ctrl-D/U` — scrolling only | the window height |

Steps 1–6 are built. Steps
4–6 are recorded so their shape is known, not because they are being built yet
— see *Deferred*.

`.` was the largest single omission and the one with reach. It is built, and it
turned out to need less than feared: commands were already values, so recording
one and replaying it is most of the work.

## What exists

Normal mode resolves `h j k l w b 0 ^ $ Space`, `gg`, `G` / `{n}G`, the arrows,
`Home`, `End`, and the doubled operator forms `dd cc yy`. Operators are `d c y`.
Single-key commands are `i a I A o O u x p P Y " : Ctrl-R Ctrl-C Esc`.

Anything else resets the pending state and does nothing.

## Step 1 — no new machinery

### The operator shorthands

Five of these already have a home. `x` and `Y` are implemented as
`resolve_as(op, motion)` — an operator the key implies rather than one the user
typed — and these are the same trick:

| Key | Is | Becomes |
|---|---|---|
| `D` | `d$` | `resolve_as(Delete, LineEnd)` |
| `C` | `c$` | `resolve_as(Change, LineEnd)` |
| `S` | `cc` | `resolve_as(Change, CurrentLine)` |
| `s` | `cl` | `resolve_as(Change, Right)` |
| `X` | `dh` | `resolve_as(Delete, Left)` |

They cost one match arm each and no new `Action`.

**Counts.** Vim's `2D` deletes to the end of the line *and* the whole line
below. bee's `D` ignores any count beyond the first line, the same
simplification already documented for `dw`. `2X` and `2s` do repeat, because
`Motion::Left` and `Motion::Right` take a count natively.

### The three that need an Action

`r`, `~` and `J` are not operator-over-motion, so they get their own actions:

```rust
Action::ReplaceChar { ch: char, count: usize },   // r{char}
Action::ToggleCase { count: usize },              // ~
Action::JoinLines { count: usize },               // J
```

Each folds its count in rather than leaning on `apply`'s repeat loop, the way
`Operate` already does: `3rx` is one replace of three characters, not three
replaces of one. `repeatable()` stays false for all three.

`r{char}` needs the keymap to hold a key back and wait for the next one, which
`"` already does via `quote_pending`. `r` gets `replace_pending` beside it.
`{count}r{char}` replaces `count` characters, and refuses — changing nothing —
if the line has fewer than `count` left, as vim does.

`~` toggles the case of the character under the cursor and moves right, so `3~`
walks three characters. It stops at the end of the line rather than wrapping.

`J` joins the line below onto the current one, replacing the newline and the
next line's leading whitespace with a single space. It does not add a space when
the current line already ends in one, or when the next line is empty. `{count}J`
joins `count` lines; `1J` and `2J` both mean "join one line below", as in vim.
The cursor lands on the join, which is where vim leaves it.

All three go through `Buffer::apply_edit`, so they are one undo step and reach
tree-sitter as ordinary incremental edits — no special casing.

### File reload

```
:e          reload from disk, refusing if the buffer is modified
:e!         reload, discarding local changes
:e <path>   edit another file
```

Reload is not "open a fresh buffer and hope". Four things have to move together
or the editor is left describing a file that no longer matches:

1. The rope is replaced from disk.
2. Undo history is reset. The old revisions describe text that is gone, and
   replaying them through `edit_raw` would corrupt the tree. Vim keeps history
   across a reload behind `'undoreload'`; bee does not, and says so.
3. The cursor is clamped — the file may have got shorter.
4. `reload_syntax` runs and `pending_edits` is cleared. The parse tree belongs
   to the old text; keeping it would feed tree-sitter edits against the wrong
   base. `:e <path>` can also change the language, which `reload_syntax`
   already handles for `:w <path>`.

`:e` on a buffer with no path is an error, not a crash. `:e` on a modified
buffer reports `unsaved changes (use `:e!` to discard)` and changes nothing,
which is `:q`'s existing wording — the precedent in this codebase.

## Step 2 — a pending argument

### Find-char

`f F t T` all wait for one more key, then resolve to a position on the current
line. They never cross a line boundary, which is what makes them cheap.

```rust
Motion::FindChar { ch: char, forward: bool, till: bool, repeat: bool },
```

`repeat` is set only by `;` and `,`, and only `t`/`T` read it. A freshly typed
`t.` from a position already next to a dot stays put; `;` from there has to skip
to the following match or it would never advance. Vim draws the same
distinction through `cpo`'s `;` flag — differential testing against vim is what
turned this up.

| Key | Direction | Lands |
|---|---|---|
| `f{c}` | forward | on `c` |
| `t{c}` | forward | just before `c` |
| `F{c}` | backward | on `c` |
| `T{c}` | backward | just after `c` |

`f` and `t` are **inclusive** — `df)` deletes the `)` too. `F` and `T` are
exclusive. This asymmetry is vim's and it is load-bearing: it is what makes
`df)` and `dF(` both do the obvious thing.

`{count}f{c}` finds the `count`-th occurrence. A find that does not hit leaves
the cursor alone and consumes the operator, so `df;` on a line with no `;`
changes nothing.

`;` and `,` repeat the last find and reverse it. The last find is editor state
rather than keymap state, because it has to survive the keymap's `reset()`.
A quirk worth preserving: in vim, `,` after `t{c}` can stick, since "till" from
a position already adjacent to the target has nowhere to go. bee does what vim
does with `cpo` unset — `,` after `t` moves to the next match, not zero
distance.

### Text objects

The first thing here that is not a motion. A motion describes *where to go from
the cursor*; a text object describes *a range containing the cursor*, which is
why `iw` cannot be spelled as a `Motion`.

```rust
pub enum TextObject {
    Word { big: bool },
    Quoted(char),      // i" i' i`
    Delimited(char),   // i( i[ i{ i<  — the open char identifies the pair
    Paragraph,
}

pub enum Target {
    Motion(Motion),
    Object { object: TextObject, around: bool },   // iw vs aw
}
```

`Action::Operate` takes a `Target` instead of a `Motion`. That is the one
breaking change in this step, and it is confined to `editor.rs` and `input.rs`.

**`i` vs `a`.** `iw` is the word; `aw` is the word plus the whitespace after it
(or before, at the end of a line). `i(` is the contents; `a(` includes the
brackets. The rule differs per object and is not worth generalising — each
object computes both.

**Resolution lives on `Buffer`**, beside the motion resolvers, for the same
reason: it needs the rope. Each returns a char range or `None`.

`Delimited` searches outward for the enclosing pair and must count nesting, or
`di(` inside `f(g(x))` deletes the wrong span. When the contents occupy whole
lines it also becomes **linewise**: `di{` on a braced body leaves the braces on
their own lines rather than collapsing them to `{}`. `a"` takes the whitespace
after the closing quote — the same rule `aw` follows. Quotes cannot nest, so `Quoted`
scans the current line only and pairs them in order — which is what vim does,
and why `ci"` behaves oddly on a line with an odd number of quotes. That
oddity is preserved rather than improved on; a smarter rule would need the
parse tree, and tree-sitter is right there for a later step.

**Not in this step:** `it`/`at` (tags) and `is`/`as` (sentences). Tags want the
parse tree; sentences want a sentence definition nobody agrees on.

## Step 3 — the remaining plain motions

`e` and `ge` (end of word), `W B E` (whitespace-delimited WORDs), `%` (matching
bracket), and the first-non-blank family: real `^`, `g_`, `+`, `-`, `_`.

`^` today is an alias for `0`. The README already flags this. Fixing it means
`Motion::LineStart` keeps meaning column zero and a new `FirstNonBlank` joins
it, rather than changing what `0` does.

`e` is **inclusive** where `w` is exclusive — `dw` and `de` deleting different
spans is the point of having both.

## Verification

`cargo test` covers the pieces. Conformance is checked separately by
`scripts/vim_differential.py`, which runs the same keys through real vim and
through bee and compares the resulting file. It is not part of `cargo test`: it
needs vim on `PATH` and drives the binary through a pty.

103 cases match vim exactly, with no divergences. Four differences it found were
fixed rather than recorded — the `t`-repeat rule above, `a"`'s trailing
whitespace, linewise inner blocks, and a **pre-existing** bug where `dG` on a
file that already ended in a newline swallowed that newline. It also caught four
of my own wrong assumptions, where bee was right and the expectation was not.

## Deferred

**Marks** (`m{a}`, `` `{a} ``, `'{a}`) — independent of everything here.

**Search** (`/ ? n N * #`) and `:s` — needs a search primitive over the rope
and a `Mode::Search` that behaves like `Mode::Command`.

**Scrolling** — `Ctrl-E`/`Ctrl-Y` (one line) and `Ctrl-D`/`Ctrl-U` (half a
window). Step 6, and specified below.

**Cursor-positioning within the window** — `H`, `M`, `L`, and the
window-repositioning `zz`/`zt`/`zb`, plus `Ctrl-F`/`Ctrl-B`. Deliberately **not**
planned. `H`/`M`/`L` are motions that need to know where the window starts and
how tall it is, and `zz`/`zt`/`zb` need to set that independently of the cursor
— which is the viewport type `Editor::scroll` currently is not. Scrolling
alone needs only the *height*, which the renderer already passes to
`scroll_to_cursor` every frame, so it is affordable in a way the rest is not.

**Case operators** (`gu gU g~`) and **indent** (`> <`) — ordinary operators;
they need `Operator` to grow, and `>` needs an indent width, which is a config
question the project has deliberately not answered yet.

## Step 4 — `.`

Repeats the last **change**. Not the last command: a motion, an undo, a yank or
a `:` command all leave `.` alone, so `dw`, `j`, `.` deletes a second word.

### What is recorded

```rust
/// The last change, in the form that replays it.
struct Change {
    /// The command that started it.
    command: Command,
    /// Keystrokes of the insert or replace session it opened, if any.
    typed: Vec<Action>,
    /// The extent a visual operator covered, so `.` can repeat it from
    /// wherever the cursor is now.
    extent: Option<Extent>,
}
```

`typed` holds `Action`s rather than a `String` because `Backspace` is part of
what was typed. Replaying "abc" when the user typed "abx", backspace, "c" would
produce the same text here but not in general — the backspace can cross the
start of the insertion and eat text that was already there.

`command` carries its own count, so `3x` then `.` deletes three more
characters. An explicit count on the `.` itself replaces it, as in vim: `3x`
then `2.` deletes two.

### What starts a change

Every action that mutates text, plus the ones that open a session:

| Kind | Actions |
|---|---|
| Immediate | `Operate` (not `Yank`), `Paste`, `ReplaceChar`, `ToggleCase`, `JoinLines` |
| Opens insert | `EnterInsert`, `EnterInsertAfter`, `EnterInsertLineStart`, `EnterInsertLineEnd`, `OpenLineBelow`, `OpenLineAbove`, and `Operate`/`OperateSelection` with `Change` |
| Opens replace | `EnterReplace` |

Yank is deliberately absent: vim does not repeat it, because `.` after a yank
almost always means "repeat the edit I made before the yank".

An immediate change is finished the moment it is applied. A session change is
finished when the mode returns to normal, which is what makes `ciwfoo<Esc>` one
repeatable unit rather than five.

### Visual repeat

`.` after a visual operator repeats it over the same **extent** from the
current cursor, which is what vim does — the selection itself is gone by then.

```rust
enum Extent {
    /// Charwise: this many characters.
    Chars(usize),
    /// Linewise: this many whole lines.
    Lines(usize),
}
```

So `vjjd` then `.` deletes three more lines, and `vlld` then `.` deletes three
more characters.

### Replaying

`.` re-applies `command`, then each entry in `typed`, then leaves the mode. The
replay must not record itself, or the second `.` would repeat the repeat and
counts would compound — a `replaying` flag guards it.

Because commands already run at every selection, `.` works with several cursors
for free.

### Not covered

**`.` after a picker paste.** The picker is an overlay with its own mode and
its result depends on what was chosen; replaying the keystrokes would reopen
it. A plain `p` repeats the same entry anyway, since choosing one moves it to
the front of the ring.

**`.` inside a macro or a count-prefixed insert** (`3ifoo<Esc>`) — bee has no
macros, and repeat-counted insert is its own feature.

### What building it turned up

Two **pre-existing** bugs, neither caused by `.` but both made obvious by it,
because a repeat starts from wherever the last one left the cursor:

- **Leaving insert did not step the cursor left.** Vim moves back onto the last
  character typed; bee only clamped onto a valid column, leaving it one to the
  right. `iAB<Esc>.` inserted in the wrong place. Leaving *visual* mode
  correctly does not step, so the fix is conditional on the mode being left.
- **`w` on the final word of a file did not move.** A file ending in a newline
  has a phantom row after it, and the clamp was happy to sit there. Vim puts
  the cursor on the last real character, which is what makes `dw` on the last
  word take the rest of the line. Only in normal mode: an operator resolves
  with `allow_eol` and needs to reach one past the end, and fixing it
  unconditionally broke `dw` on a one-word buffer.

## Step 6 — scrolling

Only the four that move the window. `H`/`M`/`L`, `zz`/`zt`/`zb` and
`Ctrl-F`/`Ctrl-B` are out of scope; see *Deferred*.

| Key | Does |
|---|---|
| `Ctrl-E` / `Ctrl-Y` | scroll the window one line down / up |
| `Ctrl-D` / `Ctrl-U` | scroll half a window down / up |

**These are not motions.** `dCtrl-D` is not a thing in vim and is not one here:
they take no operator and produce no range.

The two pairs differ in what happens to the cursor, and it is worth being
precise because it is the only interesting part:

- `Ctrl-E`/`Ctrl-Y` move the **window only**. The cursor stays on its buffer
  line, and moves solely when the window would otherwise leave it behind —
  verified in vim: with the cursor on the top visible line, `Ctrl-E` leaves it
  on the *new* top line, one further down. Scrolloff counts as "behind".
- `Ctrl-D`/`Ctrl-U` move **both**, keeping the cursor's position within the
  window. From the top line of a nine-line window, `Ctrl-D` lands four lines
  further down and the window follows.

### What it needs

The window height, and nothing else. `Editor::scroll_to_cursor(height)` is
already called by the renderer once per frame, so the editor can remember the
height it was last given rather than inventing a viewport type.

That is the whole reason this subset is cheap and the rest is not: `H` needs to
know where the window *starts* as an authority rather than a cache, and
`zz` needs to *set* it without the cursor moving. Both want the viewport to
be a real thing that the editor owns. Scrolling only needs a number the
renderer already hands over.

### Verified

Scrolling cannot be checked by comparing files — it changes what you see, not
what you have. Both editors were driven through a pty instead and their windows
and cursors compared directly, with vim given bee's `scrolloff=3` so the two are
answering the same question. `Ctrl-E`, `Ctrl-E`×3, `Ctrl-D`, `Ctrl-D`×2 and
`Ctrl-D` then `Ctrl-U` all land identically.

The bug worth recording: `scroll_to_cursor` runs every frame, so a scroll that
leaves the cursor inside the scrolloff margin is undone before it is ever drawn.
`Ctrl-E` appeared to do nothing at all. The cursor has to be pushed clear of the
margin, not merely kept inside the window.
