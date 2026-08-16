# Windows and buffers

bee holds one buffer and shows one view of it. `Editor` owns a `Buffer`, a
`Selections`, a `scroll` row and a `Syntax`, and `:e <path>` throws the old file
away to make room for the new one. There is no buffer list, so `:bn` has nothing
to cycle, and no window tree, so `:vsplit` has nowhere to put a second pane.

This adds both: a list of open buffers, and a tree of windows onto them. Two
windows may show the same buffer, editing in one moves the cursor in the other,
and closing a window leaves its buffer loaded.

## Status

**Built.**

Splitting, closing, resizing and switching windows; a buffer list with cycling,
deletion and a picker; and the `Editor` refactor all three required.
Deliberately out: moving windows around the tree (`Ctrl-W x` / `r` / `H J K L`)
and tab pages. The tree is the shape that would carry them, and neither has
earned its complexity yet.

Two things below were wrong when this was written and are corrected in place,
marked **Corrected**: what the refactor cost the tests, and what an unrecognised
`Ctrl-W` key does.

## The refactor comes first

Nothing here is reachable without moving four fields off `Editor`, and about 380
sites read them — 193 of those `ed.buffer` in tests. That sweep happens exactly
once, so the only question is what shape it lands in.

Three candidates, ranked by what they leave behind rather than what they cost:

**Park the inactive state.** Keep the fields; hold the other windows' buffers in
a side list and swap them in on focus change. Almost free, and wrong: two windows
on one file get two ropes, so `:split` on the current buffer — the most common
split there is — desynchronises on the first keystroke. Rejected on correctness.

**Accessors on `Editor`.** Replace the fields with a buffer list, a window tree
and a focus id, and turn every site into `self.buffer()` / `self.buffer_mut()`.
Mechanical and compiler-checked, but it does not end there: `apply_once`,
`operate_block` and `accept_pick` need `buffer`, `selections` and `registers`
borrowed mutably at the same time, so one split-borrow helper is not enough and
the borrow checker leaks into the call sites as a family of ad-hoc tuples. It
also leaves the window *ambient* — all 3540 lines silently mean "the focused
one", and running a command against another window means moving `focus` and
putting it back.

**A `View` bound once per command.** Chosen. The borrow split happens in one
place, and the window becomes a parameter rather than an assumption.

```rust
pub struct View<'a> {
    pub buffer: &'a mut Buffer,
    pub syntax: &'a mut Option<Syntax>,
    pub window: &'a mut Window,
    pub session: &'a mut Session,
}
```

`Editor::apply` resolves the focused window once, builds a `View`, and the
existing command implementations move from `impl Editor` to `impl View<'_>`
underneath it. Because `buffer` and `syntax` keep their names as fields, the 73
`self.buffer` and 2 `self.syntax` sites do not change at all. What changes is
`self.selections` → `self.window.selections` (36), `self.scroll` →
`self.window.scroll` (19), and the session fields → `self.session.*` (~40).
Roughly 95 mechanical edits, every one of them a compiler error until it is
done. `Editor` keeps `pub fn buffer(&self) -> &Buffer` delegating to the focused
window.

**Corrected.** That accessor does *not* leave the 193 test sites untouched, as
this first claimed: `ed.buffer` is field syntax and a method needs `ed.buffer()`.
They all changed. It was still a one-pass substitution — 117 of the 137 in
`editor.rs` are `ed.buffer().rope()`, and only five sites across the tree needed
`buffer_mut()` — but the claim that they compiled unchanged was simply wrong.

### What `View` is not

It is tempting to claim this cleaves `editor.rs` in two along a
buffer-versus-session line. It does not, and the file says so: of 55 methods,
26 touch session state, and `apply_once` — 499 lines, the dispatch match at the
centre of the editor — touches every field there is. A `View` carrying only
buffer and window would have to hand `apply_once` the session anyway.

So `View` is not a smaller `Editor`. It is `Editor` with the window resolved,
which is the whole point: the borrow split is paid once instead of at every
site, and *which* window a command runs in is written down rather than implied.
Splitting `editor.rs` by size is a separate job, and this is not it.

### Session

`Session` is what is left on `Editor` once the per-window and per-buffer state
moves out — the state one keyboard has, regardless of what it is pointed at.

```rust
pub struct Session {
    pub registers: Registers,
    pub mode: Mode,
    pub picker: Option<Picker>,
    pub status: String,
    pub last_search: Option<Search>,
    pub highlight_search: bool,
    pub line_numbers: LineNumbers,
    pub search_focus: bool,
    pub quit: bool,
    // last_find, last_change, recording, replaying, block_to_eol,
    // replaced, undo_from, pending_search_op, match_cache
}
```

A sub-struct rather than fifteen `&'a mut` fields on `View`: the borrow that
builds a `View` is then three disjoint fields of `Editor`, and adding a global
option later does not mean editing `View` as well.

`match_cache` keys on `(pattern, whole_word, edit_count, matches)` today. With
more than one buffer it must key on the buffer too, or a search count computed
in one file is served to another whose edit counter happens to agree.

### Order

Four steps, each of which leaves the tree green:

1. **`View` and `Session`, one window.** The refactor above with exactly one
   `Window` and one `BufferEntry`, no splitting and no list. Behaviour is
   preserved exactly and the test count does not move — which is the only
   evidence that a 95-site sweep landed correctly.
2. **The buffer list.** Cycling, `:bd`, `:b`, `Ctrl-^`, the picker. Still one
   window, so no fixup and no geometry.
3. **The tree.** Splitting, closing, switching, and the render loop.
4. **Resizing.** Weights already exist by then; this is command plumbing.

Steps 1 and 2 are worth doing on their own even if 3 never happens: a buffer
list is the feature people notice, and the refactor is what stops `editor.rs`
from getting harder to move every time something is added to it.

## Where state lives

| per **window** | per **buffer** | per **session** |
|---|---|---|
| `buffer`, `alt`, `selections`, `scroll`, viewport width and height | text, path, modified flag, undo history, `pending_edits`, parse tree, last cursor | mode, registers, search, picker, status, `line_numbers`, `.` state |

Two of these are worth their reasoning.

**The parse tree stays off `Buffer`.** It moves into the buffer-list entry
instead:

```rust
struct BufferEntry {
    id: BufferId,
    buffer: Buffer,
    syntax: Option<Syntax>,
    /// Where the last window to leave this buffer was looking.
    last: Cursors,
}
```

README decision #2 promises LSP that `Editor::sync_syntax` is the single drain
point for `pending_edits`, because whoever drains it destroys it for everyone
else. Putting the tree on `Buffer` would move the drain inside the buffer and
break that promise on the way past. The entry keeps the tree beside the text it
belongs to and the drain where it was.

**`line_numbers` is global, and stays global.** Vim scopes `'number'` per
window; bee does not, and this is a decision rather than a step not yet taken.

The gutter is a reading preference — how you like files to look — not a fact
about a particular view of one. Scoping it per window means the same file reads
differently depending on which pane you happened to open it in, and that every
new split inherits a setting from whichever window it was born from. Both are
answers to a question nobody asked. One value, every window obeys it, and
`:set number` needs no notion of *where*.

The cost is real and small: there is no way to number one pane and leave its
neighbour bare. Written down so the divergence is a decision rather than an
accident, in the same spirit as `:set number`'s own departure from vim's
boolean — see [line-numbers.md](line-numbers.md).

**Ids are stable, not positions.**

```rust
pub struct BufferId(u32);
pub struct WindowId(u32);
```

Both are handed out monotonically and never reused within a session. `:bd` in
the middle of the list must not silently repoint every window one file to the
left, which is exactly what a `Vec` index does. Lookup is a linear scan, because
a plausible list is under a hundred entries and a map would be machinery for
nothing.

## The tree

`src/window.rs` holds the window and the layout. Geometry is in the library:
"which pane is left of this one" is not a question about terminals, and a second
frontend should not have to answer it again.

```rust
pub struct Window {
    pub id: WindowId,
    pub buffer: BufferId,
    /// The previous buffer, for `Ctrl-^` and `:b#`.
    pub alt: Option<BufferId>,
    pub selections: Selections,
    pub scroll: usize,
    /// What the frontend last drew, which is all the scrolling commands need.
    pub height: usize,
    pub width: usize,
}

pub enum Node {
    Leaf(WindowId),
    Split { dir: Dir, children: Vec<Child> },
}

pub struct Child {
    /// Share of the parent's extent along `dir`. Siblings sum to 1.
    pub weight: f32,
    pub node: Node,
}

/// `Vertical` divides with a vertical line — children side by side, which is
/// what `:vsplit` makes. `Horizontal` stacks them.
pub enum Dir { Horizontal, Vertical }
```

The weight rides on the child rather than in a parallel `Vec<f32>`, so "one
weight per child" is a type rather than an invariant to maintain by hand across
every split and close.

Windows live in a flat `Vec<Window>` on `Editor` and the tree holds only ids at
its leaves. Splitting and closing are then pure tree surgery with no state to
carry along, and `focus: WindowId` survives both without being fixed up.

Weights are normalised fractions rather than cell counts, so a terminal resize
divides the new size in the old proportions instead of clamping panes to sizes
that no longer fit.

### Geometry

```rust
pub struct Rect { pub x: u16, pub y: u16, pub width: u16, pub height: u16 }

/// What the frontend reserves and what it can draw in — everything about
/// layout that is the frontend's business rather than the tree's.
pub struct Chrome {
    /// Columns between the children of a `Vertical` split.
    pub columns: u16,
    /// Rows between the children of a `Horizontal` split.
    pub rows: u16,
    /// The smallest pane worth handing back.
    pub min_width: u16,
    pub min_height: u16,
}

impl Editor {
    /// Lays the tree out in `area`, records each window's size, and returns
    /// one rect per window.
    pub fn layout(&mut self, area: Rect, chrome: Chrome) -> Vec<(WindowId, Rect)>;
}
```

`Rect` is four integers and no terminal. The frontend passes its own area and
its own chrome; the TUI asks for `columns: 1` — a vertical rule between panes
that sit side by side — and `rows: 0`, because the per-window status line
already separates stacked ones.

The minimums live in `Chrome` for the same reason. A pane must fit a status row
and a line of text, so the TUI's floor is `min_height: 2` — but "a status row"
is a terminal convention, and a core that hard-coded 2 would be quietly
asserting one. `min_width: 8` is the TUI's judgement about when a pane stops
being readable.

**The frontend keeps the status row.** `layout` returns whole panes; the
frontend decides how much of a pane is text and calls
`ed.size_window(id, width, text_height)`, which is `scroll_to_cursor`'s
contract with a window id and a width added — the width because a window has to
know its own for horizontal scrolling to ever be possible. Reserving the row in the core would bake a terminal
convention into geometry, and a GUI frontend would have to work around it.

**Cells are distributed by weight with the remainder going left to right**, so
rects tile the parent exactly. Anything else leaves a one-cell seam that appears
and disappears as the terminal is resized.

**A split that cannot give both children `Chrome`'s floor is refused** with a
message, rather than producing a pane with nothing in it.

### Directional switching

`Ctrl-W l` takes the nearest window on the right whose rect overlaps the focused
window's vertical span; `h`, `j` and `k` are the same rule turned. Ties break
towards the one nearest the cursor's own row or column.

This is the reason geometry is in the core rather than the frontend. The
structural alternative — walk up to the nearest ancestor split of the right
direction, descend into the neighbouring child — needs no rects and lands
somewhere surprising the moment layouts nest, because the tree's idea of
"next child" and the screen's idea of "to the right" stop agreeing.

`Ctrl-W w` and `Ctrl-W W` cycle forwards and backwards through leaves in layout
order, which is the fallback when the layout is complicated enough that
directional keys are more thought than they are worth.

## Two windows, one buffer

An edit in one window moves text under every other window showing that buffer.
Their cursors and scroll rows have to move with it.

`edit_raw` already knows the char range it replaced and the length of what
replaced it, but `Edit` only records bytes, because tree-sitter asked in bytes.
It gains the char triple beside them:

```rust
pub struct Edit {
    pub start_byte: usize,
    pub old_end_byte: usize,
    pub new_end_byte: usize,
    pub start_char: usize,      // new
    pub old_end_char: usize,    // new
    pub new_end_char: usize,    // new
    // points unchanged
}
```

`Editor::sync_syntax` becomes `Editor::settle`: the same single drain of
`pending_edits`, now feeding the parse tree *and* shifting the selections and
scroll of every other window on that buffer through the same edits. One drain,
two consumers, exactly as decision #2 describes — and when LSP arrives it is the
third.

Shifting rather than clamping is the point. Clamping to the new length keeps the
other window from pointing off the end of the rope, but its cursor slides
relative to the text every time a line is inserted above it, which is precisely
the thing a second window on the same file exists to avoid.

Undo runs through `edit_raw` and so produces `Edit`s like any other change,
which means other windows follow an undo without undo knowing they exist.

The rename reaches `main.rs`, whose event loop calls `ed.sync_syntax()` once per
key. It calls `ed.settle()` in the same place, and the comment above it — "Feed
the parse tree. LSP will hang off the same drain." — gains the third consumer it
now has.

## Window commands

```
:sp[lit] [path]     split horizontally — the new window is above
:vs[plit] [path]    split vertically — the new window is left
Ctrl-W s, Ctrl-W v  the same two
Ctrl-W h j k l      focus the window in that direction
Ctrl-W w, Ctrl-W W  cycle focus forwards, backwards
Ctrl-W c, :close    close this window
Ctrl-W o, :only     close every other window
Ctrl-W + -          taller, shorter
Ctrl-W < >          wider, narrower
Ctrl-W =            equalise every weight
```

A bare `:sp` duplicates the current window: same buffer, same cursor, same
scroll, so the split lands on the line you were reading. With a path it opens
that file in the new window, and focus follows the new window in both cases.

**Closing a window never checks for unsaved changes**, because it discards
nothing — the buffer stays in the list, and this is what "hidden buffer" means.
Closing the last window is refused; `:q` is how you leave.

**Focus after a close** goes to the sibling that inherits the space, or, when
that sibling is itself a split, to its first leaf in layout order — the
top-left-most window of the subtree that grew.

**Resizing acts on the geometry from the last layout.** `Ctrl-W +` means one
row, and a row is only a fraction of a weight once you know how many rows the
parent had, so the tree caches the size it was last laid out at. A resize before
the first frame has nothing to divide and does nothing.

## Buffer commands

```
:bn[ext] :bp[rev]   cycle the list, wrapping
:b <partial>        switch by path substring
:b#, Ctrl-^         switch to the alternate buffer
:ls, :buffers       open the picker over the list
:bd[elete][!]       delete this window's buffer
```

The list is in creation order and cycling wraps, which is vim's buffer numbering
without the numbers. `:b <partial>` matches on the path; more than one match is
an error that names them rather than a guess. `:sp <path>` and `:vs <path>`
follow the same reuse rule as `:e <path>` below — an open file is the buffer
already in the list, not a second copy of it.

`Ctrl-^` reaches `translate` as `Char('^')` with `ctrl`, which not every
terminal sends. `:b#` is the form that always works, and is why it exists
alongside the key rather than only underneath it.

`:ls` opens the existing `Picker` — a new `PickerKind::Buffer` and a source over
the list, matching by substring like the register picker, for the same reason:
paths are text where "these letters appear in order" matches nearly everything.
Accepting sets the focused window's buffer.

**It picks where vim prints.** `:ls` in vim dumps a table you then read a number
out of and retype into `:b`; here the list *is* the chooser, and the number vim
makes you carry never has to exist. The divergence is deliberate — bee has no
buffer numbers to print — and the cost is that `:ls` in a script or a pipe has
nothing to say.

### Leaving and entering

Every switch a window makes — `:bn`, `:b`, `Ctrl-^`, `:e`, the picker — is the
same two steps: the window writes its selections into the old entry's `last` and
sets `alt` to the old buffer, then takes the new entry's `last` as its
selections and scrolls to it.

Without `last`, cycling forward and back through three files loses your place in
all of them, which makes the feature something you use once. When two windows
show one buffer, the last to leave is what `last` remembers; there is no better
answer and it costs nothing to say which one wins.

A selection restored from `last` is clamped to the buffer's current length,
because the file may have been edited from another window since — the shifting
in *Two windows, one buffer* keeps live windows correct, and this catches the
one case that has no live window to shift.

Three invariants:

**The list is never empty.** Deleting the last buffer leaves a fresh `[No Name]`
in its place, so `Editor::buffer()` is always valid and no code path has to
handle a session with nothing open.

**`:bd` closes no windows.** Any window showing the deleted buffer falls to the
next one in the list. Deleting a file should not rearrange the screen — vim gets
this right and the alternative is losing a pane you were using as a reference.

Deletion also clears the id everywhere else it is held: any window whose `alt`
names the deleted buffer drops it to `None`. A `BufferId` that resolves to
nothing is the one way a stable id can be worse than an index, since the index
at least fails loudly, and clearing it at the single point of deletion is the
whole fix.

**`:e <path>` reuses an open buffer** rather than loading the file a second
time. Two live ropes over one path is the same bug the parking-lot design was
rejected for, arrived at by a different road.

### Two behaviour changes

**`:e <path>` no longer refuses on unsaved changes.** It refused because it
overwrote the only buffer there was; now the old one goes hidden with its
history and its modified flag intact, and nothing is lost. Bare `:e` — reload
this file from disk — still refuses, and still wants `:e!`, because that one
genuinely discards.

**`:q` closes the window when more than one is open**, and quits only from the
last. This is what makes `:qa` and `:wa` mean something: the README currently
describes them as "aliases until there is more than one buffer", and this is
where they stop being aliases. `:qa` checks *every* buffer for unsaved changes,
not the focused one, and names the first that fails.

### The `Ctrl-W` prefix

`Ctrl-W` is not a key, it is the start of one. `Input` gains a prefix state
beside the `[count] operator [count] motion` machine it already runs: `Ctrl-W`
arms it, the next key resolves it, and anything unrecognised drops it rather
than being swallowed. `Esc` cancels, as it does everywhere else.

**Corrected.** This first said an unrecognised key drops the prefix *with a
message*. The keymap has no way to produce one — it returns `Option<Command>`
and nothing else — and inventing an action for it would be a worse trade than
the silence. Dropping quietly is what every other unrecognised key in
`input.rs` already does.

It reports itself through `pending_display()` — the same channel that shows a
half-typed `d2`, so a pane you have armed by accident says so in the footer.

A count in front applies to the resize keys, where `3 Ctrl-W +` is three rows,
and is ignored by the rest. That is vim's behaviour and, more to the point, the
only place a count means anything here.

### Modes

Window and buffer keys are normal-mode only. Splitting mid-insert would leave an
undo group open in a window that is no longer focused, and the group would then
close around whatever was typed in the next one. Ex commands are typed from the
command line, which returns to normal before it runs anything, so `:sp` and
`:bn` are reachable from anywhere without the same problem.

## Dispatch

Window and buffer commands mutate the tree and the buffer list, so they cannot
run inside a `View` — the `View` is borrowed from the things they change. Two
entry points handle that, and both are explicit:

`Action::Window(WindowCmd)` and `Action::Buffer(BufferCmd)` arrive from the
keymap. `Editor::apply` matches on them **before** constructing a `View`, and
they run against `Editor`.

Ex commands are discovered mid-flight, inside `run_ex`, which is already running
in a `View`. `View::run_ex` returns `Option<Escalation>` for the lines it cannot
execute itself; `Editor::apply` runs it after the `View` is dropped. This is the
one awkward corner of the design, and it is written down rather than hidden: the
alternative is `run_ex` on `Editor` and a second `View` built inside it for the
lines that edit text, which puts the awkwardness in a busier place.

`accept_pick` escalates the same way for `PickerKind::Buffer`, and for the same
reason — the register picker pastes into the buffer it already has, but choosing
a buffer reaches the list the `View` was borrowed from. One `Escalation` type
covers both paths.

## Rendering

`render` becomes a loop:

```rust
let panes = ed.layout(to_core(body), CHROME);

// Every window learns its size before anything is drawn, so scrolling has
// settled by the time the first pane is formatted.
for &(id, rect) in &panes {
    ed.size_window(id, rect.width as usize, rect.height.saturating_sub(1) as usize);
}

for &(id, rect) in &panes {
    let [text, status] = split_off_last_row(rect);
    render_window(frame, ed, id, text, id == ed.focus());
    window_status(frame, ed, id, status, id == ed.focus());
    if rect.x > body.x {
        draw_rule(frame, rect);   // the column the layout reserved
    }
}
render_footer(frame, ed, footer, pending);
```

The body of today's `render` becomes `render_window`, reading a given window's
buffer, selections and scroll instead of `ed`'s. What the focus flag decides:

- **The terminal's cursor** is placed only in the focused window. There is one,
  and it goes where typing goes.
- **The cursor line** is highlighted only in the focused window, matching vim's
  `'cursorline'`. A dark bar in every pane reads as noise.
- **Selections, extra cursors and search matches draw everywhere.** They are
  real state in that window, not a hint about where focus is.
- **The status row** is reverse-video when focused, dim when not. This is the
  focus indicator, which is why the panes need no borders.

Each window's status row carries its own filename, modified marker and row:col.
The global footer keeps the mode block, messages, `:` and the live search, and
`search.md`'s rule that a live search owns the whole footer is untouched —
it owns the footer, which was never the window's row.

## Tests

The tree is pure and tests without a terminal: splitting produces the expected
shape, closing collapses a split with one child left, `:only` leaves one leaf,
weights distribute to rects that tile exactly, and directional picking lands
where the screen says it should in a nested layout.

The buffer list gets its own: `:bd` falls through to the next buffer, deleting
the last one yields `[No Name]`, `:bn` wraps, and `:e` on an open path returns
the existing id rather than a new one.

The one that matters is two windows on one buffer: edit in A, assert B's cursor
moved with the text rather than staying on a stale offset — and the same after
an undo, which is what proves the fixup rides on `edit_raw` rather than on the
editing commands.

`tests/lib_boundary.rs` grows a split session driven through the public API —
split, switch, edit in both, close — which is the embedder's proof that windows
are usable without a terminal. Its module-list literal needs `window` added, and
`the_module_list_matches_what_lib_rs_declares` will insist on it.

## Docs

README's "Single buffer, no window splits" leaves *Known gaps*. `:wa` and `:qa`
lose "aliases until there is more than one buffer". Decision #3's parenthetical
— "split windows (one per view)" as something `Cursor`-as-a-value will one day
allow — becomes something it does.
