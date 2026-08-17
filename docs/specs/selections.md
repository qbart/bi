# Selections

Visual mode needs an anchor alongside the cursor. Multi-cursor needs several of
those. Today there is one cursor and it lives on `Buffer`, which is the thing
blocking both — the README has said so since the tree-sitter step.

The move is to make a **selection** the primitive the editor works in, rather
than a position. Normal mode becomes the case where every selection is
collapsed; visual mode is one selection with room between its ends;
multi-cursor is more than one. Helix, Kakoune and Zed all landed here, and the
reason is that it turns two features into one piece of machinery.

## Status

**Built.** The cursor no longer exists on `Buffer`.

| Step | Scope | Needs |
|---|---|---|
| **1** ✅ | The selection model; cursor leaves `Buffer` | this file |
| **2** ✅ | Visual mode `v` `V`, Replace mode `R` | step 1 |
| **3** ✅ | Multi-cursor | step 2 |

Blockwise visual (`Ctrl-V`) is **not** in step 2 — see *Deferred*.

## The model

```rust
/// A range of the buffer with a direction. Collapsed when the ends meet,
/// which is what normal mode is.
pub struct Selection {
    /// Where the selection was started. Fixed while the head moves.
    pub anchor: Cursor,
    /// Where the cursor is. Motions move this end.
    pub head: Cursor,
}

/// Never empty. Sorted by position, never overlapping.
pub struct Selections {
    list: Vec<Selection>,
    /// The one the viewport follows and the status line reports.
    primary: usize,
}
```

Three invariants, enforced in one place rather than at every call site:

1. **Never empty.** There is always a cursor. Removing the last selection is
   not expressible.
2. **Sorted by head position.** Applying an edit per selection needs a defined
   order, and back-to-front is the only order that works (below).
3. **Never overlapping.** Two cursors that collide become one. Without this,
   typing with two cursors on the same character inserts twice.

`Selection::range()` returns the char range low..high regardless of direction,
which is what operators want. Direction only matters for `o` (swap ends) and
for which end a motion moves.

## Where it lives

`Editor` owns `Selections`. `Buffer` loses its `cursor` field entirely.

This is the expensive half. Every mutating method on `Buffer` currently reads
`self.cursor` and writes it back; each instead takes the position it acts on and
returns where the cursor ends up:

```rust
// before
pub fn insert_str(&mut self, text: &str)
pub fn operate(&mut self, op: Operator, target: Target, count: usize) -> Option<Entry>

// after
pub fn insert_str(&mut self, at: Cursor, text: &str) -> Cursor
pub fn operate(&mut self, at: Cursor, op: Operator, target: Target, count: usize)
    -> Option<(Entry, Cursor)>
```

The pure motion resolvers already take a `Cursor` and return one — they were
written that way so an operator could ask where a motion *would* land. That
half of the work is already done, which is what makes this affordable at all.

`Buffer` keeps the rope, history, `pending_edits` and the motion resolvers. It
becomes a text store with no notion of where anyone is looking, which is also
what a second view of the same file will need.

## Applying a command to N selections

Back to front, highest position first — and that is necessary but not
sufficient.

An edit at position 40 does not disturb a selection at position 5, but an edit
at 5 shifts everything after it. Descending order therefore keeps each
selection's **edit position** valid: it is still pointing at the right text when
its turn comes.

The positions that come *back* are a second problem, and the first cut of this
got it wrong. A selection dealt with early sits above the ones still to come, so
every later edit shifts it — the head it reported is stale the moment a lower
edit lands. After each selection is handled, the buffer's length delta has to be
applied to everything already processed. Without it, a multi-cursor insert
leaves the second cursor one character short, the third two, and so on.

So: descending order removes the need to correct the *inputs*; a running delta
corrects the *outputs*. Both, or it drifts.

After the pass: re-sort, merge any selections that now overlap, and clamp each
to the buffer.

One command is still one undo step. The group closes after all selections have
been visited, not after each — otherwise `u` would walk back through a
multi-cursor edit one cursor at a time.

## History

`History` records a cursor position per revision so undo can restore it. It
records the whole selection set instead.

This matters more than it sounds: undoing a multi-cursor edit and finding a
single cursor makes redo unusable, and it is the difference between multi-cursor
being a feature and being a party trick. The change is contained — `History`
already stores a `usize` per revision and it becomes a `Selections`.

Done in step 3, as promised. Doing it properly also *simplified* `Buffer`:
`apply_edit` used to take a cursor purely so history could record one, and with
several selections the group's first change comes from whichever happened to be
highest — a position that is not the state to restore. `Editor` now captures the
set when a group opens (`undo_from`) and passes both ends at `commit`, so
`apply_edit` needs no cursor at all.

What comes back is **collapsed** unless the editor is in visual mode. The pair
a revision recorded is whatever was live when the change started, which for a
visual or blockwise operator is a selection with room in it — room that means
nothing once the mode is normal again, and that the renderer would otherwise
draw as a selection the user cannot act on. Vim leaves no selection behind an
undo. The *number* of selections still survives, which is the part that makes
undoing a multi-cursor edit give the cursors back.

`History` stores plain `(anchor, head)` pairs rather than a `Selections`, which
keeps it a leaf module: `Selections` knows about `Cursor`, and importing it
there would tie the undo tree to the editor's idea of what a cursor is.

## Step 2 — visual and replace mode

### Visual

```rust
Mode::Visual(VisualKind)   // Char | Line
```

- `v` starts charwise from the cursor; `V` linewise. Pressing the same key
  again, or `Esc`, collapses back to normal mode.
- Motions move the **head**; the anchor stays put. Every existing motion and
  text object works unchanged, which is the payoff for having done step 2 of
  `motions.md` first: `viw`, `vi(`, `vap` all work the day this lands.
- Operators (`d` `c` `y`) apply to the selection, then collapse it and return
  to normal mode. `x` is `d`, `s` is `c`.
- `o` swaps anchor and head, so the other end can be adjusted.
- A charwise visual selection is **inclusive** of the character under the head,
  matching vim. Linewise covers whole lines regardless of column.

Operators in visual mode go through `Action::OperateSelection` rather than a
new `Target` variant. The range comes from the selection, which lives on
`Editor`, and putting a `Target::Selection` into `Buffer`'s target machinery
would have meant teaching `Buffer` about selections — the one thing step 1 took
out of it. `Editor` works the range out and hands it to `Buffer::operate_range`.

`i`/`a` in visual mode name a text object and make it the *selection*
(`Action::SelectObject`) rather than operating on it, which is what makes `viw`
and `vi(` work.

Two things that only show up when you try it, both caught by differential
testing against vim:

- **`Esc` has to be claimed by visual mode.** Falling through to normal mode's
  `Esc` only clears pending keymap state and resolves to no command, so visual
  mode kept running while the user believed they had left it.
- **A collapsed selection still covers something in visual mode** — one
  character for `v`, a whole line for `V`. The renderer skipped collapsed
  selections, so `V` on a single line highlighted nothing.

### Replace

```rust
Mode::Replace
```

`R` overwrites instead of inserting, until `Esc`. Backspace restores what was
there rather than deleting — so the session has to remember the characters it
overwrote, as a stack on `Editor` (one entry per selection per keystroke, so it
works with several cursors).

A whole `R` session is **one undo step**, which means replace mode holds the
undo group open until `Esc` exactly as insert mode does. Missing that made every
keystroke its own revision.

Overwriting stops at the line end: typing past it appends rather than eating the
newline, which is what vim does. `r{char}` is unrelated and already built.

## Step 3 — multi-cursor

With the model in place this is mostly input and rendering. Every command
already applies to N selections, so what is left is creating and destroying
them, and drawing more than one cursor.

Rendering is the part with no precedent in this codebase: a terminal has exactly
one real cursor. The primary selection gets it; every other head is drawn as a
styled cell — reversed video — by the same span-patching path
`fill_line` already uses for the cursor line.

Vim has no multi-cursor and so offers no bindings to copy. bi's:

| Key | Does |
|---|---|
| `Ctrl-N` | add a cursor at the next occurrence of the word under the cursor |
| `Ctrl-Alt-Down` / `Ctrl-Alt-Up` | add a cursor on the line below / above |
| `Esc` | collapse to the primary cursor |

Chosen over VSCode's `Ctrl-D` because that is vim's half-page scroll, which
`motions.md` step 6 still wants, and over a `g` prefix because `gj`/`gk` are
vim's display-line motions and soft wrap will want them. `Ctrl-N`/`Ctrl-P` are
picker-only today, so normal mode has them free. This is also the first use of
the `alt` modifier `Key` has been carrying since the library split.

## Deferred

**Blockwise visual (`Ctrl-V`).** A block is not a range — it is a rectangle,
and it does not fit `Selection` without either a third mode flag on every
selection or one selection per line. The second is expressible in this model
and is how Kakoune does it, so blockwise becomes a way of *creating* several
selections rather than a new kind of one. Worth doing after multi-cursor
exists, not before — which it now does. Specified in
[blockwise.md](blockwise.md), where the rectangle is derived from the primary
selection's corners and materialises into selections only when something acts
on it.

**Split windows.** Two views of one file need two selection sets, one per view.
This design allows it — `Selections` is not a singleton on `Editor` by
necessity, only by current need — but the viewport work (`Editor::scroll` as a
bare row index) has to land first.

**`gv`** — reselect the last visual range. Needs the previous selection set kept
after collapse. Cheap once the model exists.
