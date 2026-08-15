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

**Specified**, not yet built.

| Step | Scope | Needs |
|---|---|---|
| 1 | The selection model; cursor leaves `Buffer` | this file |
| 2 | Visual mode `v` `V`, Replace mode `R` | step 1 |
| 3 | Multi-cursor | step 2 |

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

Back to front, highest position first.

An edit at position 40 does not disturb a selection at position 5, but an edit
at 5 shifts everything after it. Iterating in descending order means each
selection's recorded position is still valid when its turn comes, with no offset
arithmetic at all. Ascending order would require rewriting every later selection
after every edit — the bug that makes bolted-on multi-cursor implementations
drift.

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

`Action::Operate` already takes a `Target`. Visual mode adds one more:
`Target::Selection`, meaning "whatever is selected". That keeps operators from
needing to know what mode they are in.

### Replace

```rust
Mode::Replace
```

`R` overwrites instead of inserting, until `Esc`. Backspace restores what was
there rather than deleting — so the session has to remember the characters it
overwrote, as a stack on the mode.

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

Vim has no multi-cursor and so offers no bindings to copy. bee's:

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
exists, not before.

**Split windows.** Two views of one file need two selection sets, one per view.
This design allows it — `Selections` is not a singleton on `Editor` by
necessity, only by current need — but the viewport work (`Editor::scroll` as a
bare row index) has to land first.

**`gv`** — reselect the last visual range. Needs the previous selection set kept
after collapse. Cheap once the model exists.
