# The file tree

`bee src/main.rs` opens a file. `bee .` fails, because `Buffer::open` reads a
path into a rope and a directory is not text. There is no way to see what a
project contains without leaving the editor, and no way to create, rename or
delete a file from inside it.

This adds a tree: a pane that shows a directory, expands and collapses it, opens
what you select, and edits the filesystem.

## Status

**Built.**

The `Tree` model, the `Content` split, the ex-line lift, the keymap, the
rendering and the three file operations. Deliberately out: filtering the tree,
git status, file watching, editing the listing to move files, and multi-select
— all listed at the end with their reasons.

Five things below were wrong when this was written and are corrected in place,
marked **Corrected**: what `View` does with a pane's height, what `Escalation`
had left to do, what the root row is called, what a relative root does to `-`,
and which trees a file operation refreshes.

## The tree is not a buffer

The obvious design is that a directory becomes a buffer whose text is the
listing — vim's netrw, and the reason `:e .` works there. It was the
recommendation here until file operations came into scope, and file operations
are what kill it.

Under that design a rename is: perform the syscall, regenerate the listing, and
push the new text through `edit_raw`. But `edit_raw` is the one function a
read-only tree has to be guarded at, so the regeneration needs a bypass. It
lands in the undo history, or history needs suppressing for this buffer kind. It
bumps the edit counter and appends a whole-file `Edit` to `pending_edits`, which
`Editor::settle` then applies to every other window on that buffer — scattering
their cursors, because a regenerated listing shares no offsets with the old one.
Five exception clauses for one keystroke, and each of them reads "unless this
buffer is not really a buffer".

The inheritance argument cuts the other way too. A tree buffer would get the
gutter, the syntax slot, selections and multi-cursor for free, and every one of
those then needs suppressing — so the saving is a list of features to turn off.

Two other designs were weighed and rejected:

**A `Mode::Tree`.** Cheapest, since dispatch already switches on mode. Wrong:
mode is session state and a tree is a property of one window. Split a tree beside
a file and focus has to rewrite the mode on every switch and restore it coming
back — a second copy of a fact the window already holds, free to disagree with
it.

**A `Vec<Tree>` on `Editor`, windows holding a `TreeId`.** This is what buffers
do, and the reason buffers do it does not apply. Two windows share one `Buffer`
because two ropes over one path would diverge and one of them would win the next
`:w`. A tree has no document to diverge: no text, no unsaved state, no undo.
What two tree panes on one directory would share is expansion state, and
expansion is a view property like scroll — two panes wanting different
expansions is the reason you opened the second one. So the tree lives in the
window, and closing the window discards it because there is nothing to lose.

What the tree *is* has a precedent in this tree already, and it is not `Buffer`.
`picker.rs` opens with "State only — it does not draw. `ui.rs` reads what it
holds and renders it. That split is what makes the whole state machine testable
without a terminal." A tree is the same species: a selectable, scrollable list
that a frontend reads and draws. It even wants the same three fields — `selected`,
`scroll`, and a `scroll_to_selected(height)` the frontend calls with the room it
gave.

## What a window shows

`Window` currently names a buffer and carries the selections and scroll for it.
Both become one field:

```rust
pub struct Window {
    pub id: WindowId,
    pub content: Content,
    /// What this window showed before. `Ctrl-^` swaps them.
    pub alt: Option<Content>,
    pub height: usize,
    pub width: usize,
}

pub enum Content {
    Text(Text),
    Tree(Tree),
}

/// A window's view onto a buffer: which one, and where in it.
pub struct Text {
    pub buffer: BufferId,
    pub selections: Selections,
    pub scroll: usize,
}
```

The selections and the scroll row move *inside* the `Text` variant rather than
staying beside `content`. Leaving them on `Window` would give every tree pane a
`Selections` that means nothing and a `scroll` that duplicates the one the tree
keeps — the same "dead fields for one kind" smell that disqualified the buffer
design above, arrived at from the other side.

It also buys a guarantee. `Editor::view` builds a `View` by borrowing a buffer,
a syntax slot, a window and the session at once; with selections inside the
variant it can bind them directly and return `None` for a tree window, so **a
`View` cannot be constructed on a tree**. The editing commands do not need to
know trees exist.

`View` stops holding `&mut Window` to get there, because it cannot hold the
window and the selections inside it at the same time. It destructures once:

```rust
pub struct View<'a> {
    pub id: BufferId,
    pub buffer: &'a mut Buffer,
    pub syntax: &'a mut Option<Syntax>,
    pub selections: &'a mut Selections,
    pub scroll: &'a mut usize,
    pub window: WindowId,
    pub height: &'a mut usize,
    pub width: usize,
    pub session: &'a mut Session,
}
```

That is the borrow split `View` exists to pay once, applied one level deeper. The
37 `self.window.selections` and 10 `self.window.scroll` sites become
`self.selections` and `*self.scroll` — shorter than what they replace.

**Corrected.** This first had `height` and `width` as plain copies, on the
grounds that "a command reads the room it has and never resizes it". Half wrong:
`scroll_to_cursor` *records* the height it was handed, which is where `Ctrl-D`
later reads the page size from, so a copy loses the write and every scrolling
test fails. `height` is borrowed mutably; `width` really is a copy, because only
the frontend sets it. Both come out of the same destructure — `content` and
`height` are different fields of `Window`, so splitting them is what lets the
view hold the selections and the geometry at once.

**`alt` carries a whole `Content`, not a `BufferId`.** It has to: opening a file
from a tree in a single-window session replaces the tree, and `Ctrl-^` must bring
it back with its expansion intact rather than re-reading the directory from
scratch. Widening `alt` is also the more honest type — "what this window showed
before" was never specifically a buffer.

`BufferEntry::last` stays as it is. `alt` remembers what *this* window showed;
`last` is where the file was when some *other* window left it. They answer
different questions and windows.md's reasoning for `last` is untouched. Leaving a
buffer still writes `last`, whether what replaces it is another buffer or a tree.

**Corrected.** This first said selections restored from `alt` are clamped, on
the grounds that a parked `Content::Text` is not a live window for `settle` to
shift. True, and beside the point: `Ctrl-^` onto a buffer goes through `show`,
which restores from the entry's `last` and clamps there already. `alt` is read
for its *buffer id* in that case and nothing else.

Where `alt` is read whole is the case it was widened for. `Ctrl-^` with a tree
parked swaps the window's content back, expansion and all, and the file it
displaces becomes the new alternate. That swap has to be checked before the
slot is taken — taking first and matching after empties it for a buffer
alternate too, which is exactly what the existing `Ctrl-^` test caught.

## The tree

`src/tree.rs`. State and filesystem, no terminal — the same rule the rest of the
library keeps, enforced by `tests/lib_boundary.rs`, whose module list grows
`tree`.

```rust
pub struct Tree {
    root: PathBuf,
    /// Directories the user has opened. Paths rather than indices, so refreshing
    /// from disk cannot silently re-point them at whatever moved into the slot.
    expanded: BTreeSet<PathBuf>,
    /// The flattened visible tree, top to bottom. Row index *is* cursor row.
    rows: Vec<Row>,
    selected: usize,
    scroll: usize,
    show_hidden: bool,
}

pub struct Row {
    pub path: PathBuf,
    /// The final component alone. The full root path is [`Tree::root`].
    pub name: String,
    pub depth: usize,
    pub kind: Kind,
    /// A directory the user has opened. Always false for the other kinds.
    pub open: bool,
}

pub enum Kind { File, Dir, Link }
```

`rows` is rebuilt whole on expand, collapse, refresh and hidden-toggle, for the
reason `Picker::refilter` gives for the same choice: at the sizes involved that
is far cheaper than the machinery to avoid it.

**The core emits structure, the frontend picks glyphs.** `Row` says depth and
kind; it does not say `▾` or `│  ├─ `. A GUI frontend draws indent guides and
icons from the same three fields, which is README decision #6 — capture names,
not styles — applied to a second subsystem.

**Corrected.** The root row carries the root's *final component*, not its
display path. The full path is what the status row shows, and repeating it at
the top of a pane eight columns wide was the same fact twice, in the place with
the least room for it.

**Corrected.** The root is resolved with `canonicalize` on the way in. `bee .`
would otherwise root at `"."`, whose parent is `""` rather than the directory
above it — leaving `-` nowhere to go in the one case the key exists for. The
tests missed this because `Path` comparison normalises a *trailing* `.` away, so
every scratch-directory spelling of it passed. Running the editor is what
found it.

**Order is directories first, then by name.** Case-insensitively, so `README.md`
does not sort away from `readme-draft.md`.

**Hidden entries are those whose name starts with `.`,** filtered unless
`show_hidden`. `.gitignore` is deliberately not consulted: it would make the
tree's contents depend on a git dependency and a per-directory rule stack, and
`target/` — the case that motivates it — is one collapsed row.

**Symlinks are shown and never expanded.** A symlinked directory is a `Kind::Link`
row that opening follows as a file. Following them into the tree means a cycle
check on every expand, and the answer to "what does the loop look like" is worse
than the answer to "why will this one not open". Kind comes from
`symlink_metadata`, so the property holds by construction rather than by a guard.

**An unreadable directory expands to nothing** and puts the error in the status
line. One bad permission should not fail the tree around it.

No cap on directory size. Fifty thousand entries make fifty thousand rows, and
rendering is already viewport-bounded (README decision #7), so the cost is the
`read_dir` and the `Vec`. Worth knowing, not worth solving yet.

## Keys

The tree keymap is **complete, not an overlay**. In a window holding a tree,
`Input` dispatches to it and nothing falls through to normal mode except what it
explicitly re-exposes.

That direction matters. A denylist — "normal mode, minus the keys that edit" —
is a list that has to be revisited every time a key is added to the editor, and
is silently wrong until someone notices. An allowlist is closed: a key nobody
taught the tree does nothing, which is the safe failure for a pane sitting on a
filesystem.

```
j k             down, up a row                       [count] applies
gg G            first, last row
Ctrl-D Ctrl-U   half a page
l / Right       expand a directory; on a file, open it
h / Left        collapse; already collapsed, go to the parent row
Enter           a directory toggles, a file opens
-               re-root at the parent directory
+               re-root at the selected directory
R               re-read from disk
gh              show or hide dotfiles
a               create — prefills the command line
r               rename — prefills the command line
dd              delete, with no `:` line in between
Ctrl-W …        every window key, unchanged
Ctrl-W e        a tree beside this one, in a pane of its own
:               the command line
Ctrl-^          the alternate content, tree or file
```

Nothing else. `/` is not in the list because the tree has no search, and neither
are `i`, `a`'s normal-mode meaning, `p` or `x` — an allowlist does not have to
name what it excludes.

`-` and `+` are inverses, and the pair is the reason the tree does not only get
wider as you use it: `+` scopes to the directory you are standing in — the one
holding the file, when the cursor is on a file — keeping expansion so that `+`
then `-` is a round trip rather than a reset. `.` was the other candidate for
scoping in and was turned down for the reason `gh` beat it earlier: it means
repeat in every other pane, and a key that changes meaning per pane is worse
than one that reads as the opposite of the key beside it.

`h` collapsing and then walking to the parent is one key doing the thing you
meant either way: on an open directory there is something to close, and
everywhere else the only sensible upward move is to the parent row.

`-` re-roots at the parent and selects the directory it just left, expanded — so
`-` then `l` is a round trip. There is no `..` row, because a row that is not a
file is a row every motion has to know about.

`gh` sits beside `gg` under the `g` prefix `input.rs` already runs. `.` was the
alternative and was rejected: it means repeat everywhere else in the editor, and
a key that changes meaning per pane is worse than one more `g`.

**`-` in a text window opens the tree** at the current file's directory with that
file selected — the same path as `:e <dir>`, so it parks the buffer in `alt` and
`Ctrl-^` goes straight back. One key, two directions: down into a file from the
tree, back out to the tree from the file. On a `[No Name]` buffer it roots at the
working directory. `-` is currently bound only after `Ctrl-W`, so nothing is
displaced.

Because no key in that list enters insert or visual mode, and `Ctrl-W` is
normal-mode only, **a tree can never be focused in insert or visual mode**. Mode
stays `Normal` across every focus change, and the footer never announces a mode
with nowhere to type. That falls out of the allowlist rather than being enforced
anywhere.

## Opening a file

Enter on a file opens it in the **last-focused window that is not this one**, and
the tree stays where it is. With no other window it opens in place, and the tree
goes to `alt`.

So `:vs .` is a persistent sidebar and `bee .` is netrw, out of one rule and no
sidebar concept. The window tree needs no special case, `:only` needs no special
case, and a tree pane resizes and closes like anything else.

This needs `Editor::previous: Option<WindowId>`, set on every focus change. If it
names a window that has since closed, or one holding a tree, the target is the
first leaf in layout order holding text; if there is none, the file opens in
place. `Ctrl-W p` falls out of the same field and is not specified here.

Focus follows the file. You asked to open it.

**Last-focused rather than an origin recorded on the tree.** A sidebar could
remember the window it was opened from and always hand back to that one. It
would be the same answer nearly every time — you reach a tree by moving focus to
it, which makes the pane you left the previous one — and worse the rest of the
time, because with two files open the pane you were *last* reading is the one
you mean, not the one you happened to press the key in ten minutes ago. One
field, no per-tree state, and it follows you.

### The sidebar shortcut

`Ctrl-W e` is that layout in one key: split vertically, root the new pane at the
current file's directory, select the file, and narrow it to
`Chrome::tree_width`. Under the window prefix because it makes a window, which
is where every other key that makes one lives.

**There is one tree, and `Ctrl-W e` toggles it.** Pressed with a tree open —
from the tree or from anywhere else — it puts that one away rather than opening
a second. Two trees are two of the same thing, and the second is never the one
you wanted; a tree is a place you look things up, not a document you might want
two views of. `-` follows the same rule from the other side: with a tree open it
moves focus to it, and only opens one when there is none.

Closing means closing the window, except when it is the last one — that can
never close, so it shows a buffer instead. Which is what Enter on a file already
does from a `bee .` session, and leaves a session that still has a window in it.

The escape hatch is still there: `:e <dir>`, `:sp <dir>` and `:vs <dir>` name a
path outright, and someone who types two of them has asked for two trees.

The width is `Chrome`'s rather than the core's, for the reason `min_width` is:
how wide a tree wants to be is a judgement about reading, and a core that
hard-coded thirty columns would be asserting one on every frontend. It is a
*starting* width — the pane becomes a weight like any other and keeps its share
of the terminal, because a genuinely fixed-width pane is a concept `Layout` does
not have and this did not need.

On a window already holding a tree it duplicates that tree's root, the way
`Ctrl-W v` duplicates a window rather than refusing.

## File operations

Three ex commands, and the tree keys are prefills over them:

```
:create <path>          an empty file; a trailing / makes a directory
:rename <old> <new>     also how you move a file
:delete[!] <path>       ! for a directory with anything in it
```

Making them ex commands rather than tree-only actions is the point. They are
typeable without a tree open, they are testable without one, and the tree keys
become the thing they should be — a way to fill in the path you are already
looking at.

`a` opens the command line on `create <selected directory>/`. `r` opens it on
`rename <path> <path>` with the cursor at the end, so backspacing the basename
and typing a new one is the rename.

**A prefilled command line is the confirmation.** The editor has no prompt
machinery and gains none here: you see the path, and Enter is the assent. Where
that is not enough — a directory with contents — the guard is `!`, which is how
`:q` and `:e` already refuse.

**`dd` is the exception, and deletes outright.** It is vim's spelling for
removing the thing under the cursor, and the fast path is what a file tree is
for; a `:` line in front of every deletion is the kind of friction that gets a
feature stopped being used. The trade is real and worth writing down: there is
no undo for the filesystem and nothing is moved aside first, so a mistyped `dd`
on a file is a file gone.

Three things blunt it. `dd` is a whole key, so one `d` does nothing and any key
that is not the second `d` drops it rather than being swallowed — the rule
`Ctrl-W` already follows, and the reason an armed `d` shows in the footer.
`:delete`'s two guards survive: a directory with anything in it and an open
buffer with unsaved changes both still want `:delete!` typed out in full. And
the root row is refused, because it is the directory you are standing in and
never what `dd` meant.

There is no key for a *prompted* delete any more. `:delete <path>` is still
there for the times you want to read the path before agreeing to it.

**`:create` makes intermediate directories** and refuses a path that exists.
Refusing to create `src/a/b/c.rs` because `src/a/b` is missing is a message that
tells you to type two more commands you already asked for.

**`:rename` retargets an open buffer.** Any buffer whose path is `<old>` takes
`<new>`, and its syntax is re-picked, since the extension may have changed.
Leaving it pointing at a path that no longer exists means the next `:w` recreates
the file under its old name.

**`:delete` leaves an open buffer open.** Its text and history are intact and it
simply no longer has a file, exactly as if it had never been saved. Deleting the
file out from under a pane the user is reading is not a reason to close it.

**Every tree refreshes.** After any of the three succeed, every open tree
re-reads, keeping expansion and holding the selection on the same path — or on
the nearest surviving row when the selection is what was deleted.

**Corrected.** This first said "every window whose tree contains the affected
parent directory". Every tree, in the end: one that cannot see the change
re-reads to exactly the rows it had, so telling them apart would have bought
nothing but the bookkeeping to do it.

## Dispatch, and what the tree forces

`Action::Tree(TreeCmd)` is matched in `Editor::apply` **before a view is built**,
beside `Action::Window` and `Action::Buffer`, and for the same reason: it changes
window content and may open buffers, which a `View` is borrowed from.

```rust
pub enum TreeCmd {
    Select { down: bool, count: usize },
    First, Last,
    HalfPage { down: bool },
    Expand, Collapse, Enter, Up,
    Refresh, ToggleHidden,
    /// `a` `r` `d` — fills the command line and hands over.
    Prompt(FileOp),
}
```

`Input` needs to know which keymap to run, so `on_key(key, &Mode)` becomes
`on_key(key, Context { mode, content })` where `content` is
`ContentKind { Text, Tree }` — an enum rather than a bool, because the next pane
kind should be a variant and a compiler error, not a second flag.

### Ex lines need parsing before dispatch

`View::run_ex` is reached from inside a view, and returns `Escalation` for the
lines that need more than one. A tree window has no view at all, so `:` typed in
one currently has nowhere to land.

The fix is to lift the parse: `run_ex` splits into a parser that turns a line
into an `ExLine`, and two runners. Lines that do not need a buffer — the window
and buffer commands, `:q`/`:qa`/`:wa`, the three file operations — run against
`Editor`. Lines that edit text run in a view, and report "no buffer in this
window" when there is none.

windows.md called the escalation "the one awkward corner of the design, and it is
written down rather than hidden". This is the corner being straightened, and the
tree is what forces it: with the parse out front, `Escalation` is no longer how
ex commands reach the editor. It survives only for `accept_pick`, where a
register pick pastes into the buffer the view already holds and a buffer pick
reaches the list that view was borrowed from.

**Corrected.** `Escalation` does not survive at all. Running the editor found
that `:` did nothing in a tree pane: `EnterCommandMode` and the picker keys are
*session* state that was living inside `View`, so with no view they never ran.
They move out to `Editor::run_session_action`, and once the picker's keys are
there `accept_pick` follows — it sends a buffer pick to the list and a register
pick through `in_view`. Nothing is left that a view discovers mid-flight and
hands back, so the type is gone.

The lesson is worth keeping. "Needs a view" is not the same question as "is an
editing command": the command line and the picker look like editor features and
are session state, and the only thing that tells them apart is a pane with no
buffer to run them against.

## What this breaks

**`Editor::buffer()` returns `Option<&Buffer>`.** Its doc comment today reads
"Always valid: the buffer list is never empty and every window names a buffer in
it." The first clause stays true — the list is still never empty and
`[No Name]` still backfills a `:bd` of the last entry. The second stops being
true, and the type should say so rather than the accessor panicking on a pane
the user can open with one keystroke.

The same goes for `syntax()`, `selections()`, `scroll()`, `cursor()`,
`cursor_row()` and `cursor_col()`. The 164 sites reading `ed.buffer()` are a
mechanical sweep — 158 of them tests in `editor.rs` and 4 in the renderer — and
136 are `ed.buffer().rope()`, which collapse into one test helper.

**`Pane` becomes an enum**, since the renderer has two things to draw:

```rust
pub enum Pane<'a> {
    Text { window: &'a Window, text: &'a Text, buffer: &'a Buffer, syntax: Option<&'a Syntax> },
    Tree { window: &'a Window, tree: &'a Tree },
}
```

**`Editor::settle` walks text windows only.** Its filter becomes "windows whose
content is `Text` on this buffer", which is the same set it walks today.

**`:bd` in a tree window is an error** — there is no buffer in it to delete.
`:bn`, `:bp`, `:b <partial>`, `Ctrl-^` and an accepted `:ls` all *work*: they set
this window's content to a buffer and park the tree in `alt`, which is what
asking to see a buffer here means. `:w` in a tree window is an error.

## Rendering

`render_window` matches on `Pane` and gains `render_tree` beside it. A tree row
is indent, a marker, and a name: `▾` for an open directory, `▸` for a closed one,
two spaces for a file, `@` after a link. Two columns of indent per level.

- **The gutter is not drawn.** `number` is a global option (number.md), and it
  stays global — a tree simply has no line numbers to show, because a row is not
  a line. This is the option having nothing to say here, not a per-window
  override arriving by the back door.
- **No syntax highlighting**, and no syntax slot to hold any. Directories,
  links and the selected row are the only colour.
- **The selected row is highlighted in every tree pane**, brighter in the focused
  one. Unlike a text cursor, it is where the *next* Enter goes, so an unfocused
  tree hiding it would make `Ctrl-W h` a guess.
- **The terminal cursor** goes at the first character of the selected row's name
  when a tree is focused, so the terminal's own cursor agrees with the highlight.
- **There is no status row.** A tree pane keeps its whole rect.

  **Corrected, twice.** This first gave the tree the root's *display path* and a
  `row/total` count. A thirty-column sidebar spent all of them truncating the
  path into something unreadable, so it became the last component — and then the
  last component was exactly what the pane's own first row was already saying,
  which left the row saying nothing a glance at the pane did not. So it is gone,
  and `render` hands a tree pane the row the status would have taken.

  What the row was really carrying was the mode, and that belongs to the window
  with the cursor in it. A tree usually has the cursor and no text to type into,
  which is another way of saying the same thing.

The footer is untouched. It carries the mode, messages and the `:` line, none of
which are per-window.

## Startup and ex

`Editor::open` branches on `is_dir`. A directory yields an editor whose one
window holds a tree and whose buffer list holds a single `[No Name]` — the list
is never empty, so nothing downstream learns that the session started on a
directory.

```
bee .            a tree on the current directory
bee src/         a tree on src/
:e <dir>         this window shows a tree; its old content goes to alt
:sp <dir>        a new window holding a tree
:vs <dir>        the same, beside
-                in a text window: the tree on this file's directory
```

`:e <dir>` deliberately reuses `:e`. A path is a path, and a separate `:tree`
command would mean remembering which one a given path wants.

## Tests

The tree is pure state over a real directory, and the existing `Scratch` helper
pattern extends to one.

- Expanding a directory inserts its children below it at depth+1; collapsing an
  ancestor hides every descendant.
- Expansion survives `R`, including for a directory that has gained a file.
- `gh` reveals dotfiles and hides them again; the selection clamps rather than
  resets when the list shrinks under it, as the picker's does.
- `-` re-roots and lands selected on the directory it left, expanded.
- A directory that cannot be read expands to nothing and reports why.

File operations, each of which is an ex command and needs no tree:

- `:create a/b/c.rs` makes the intermediate directories; a second one refuses.
- `:create dir/` makes a directory.
- `:rename` on an open file moves the buffer's path with it, and re-picks syntax
  when the extension changes.
- `:delete` refuses a non-empty directory and takes it with `!`; an open buffer
  survives the deletion of its file.
- Every tree showing the parent directory refreshes.

The ones that matter are the integration cases, because they are where the
design's two claims live:

- Enter on a file with one window replaces the content and parks the tree in
  `alt`; `Ctrl-^` brings it back with its expansion intact.
- Enter on a file with a split sends the file to the previously focused window
  and leaves the tree alone.
- No `View` can be built on a tree window — `Editor::view` returns `None`, which
  is the compiler-checked form of "the editing commands never see a tree".

`tests/lib_boundary.rs` grows a tree session driven through the public API —
open a directory, expand, open a file into a split, rename it — which is the
embedder's proof that the tree needs no terminal. Its module-list literal needs
`tree`, and `the_module_list_matches_what_lib_rs_declares` will insist on it.

## Docs

README's status line and feature list gain the tree, and the module table gains
`tree.rs`. Decision #6 — capture names, not styles — is worth a sentence noting
that `Row` is the same rule applied to a second subsystem. *Known gaps* is
untouched: it never claimed a tree, and the things this leaves undone are listed
under *Deliberately out* below rather than promoted to gaps.

windows.md needs two corrections in place, in the style it already uses for its
own: `Window` no longer holds `selections` and `scroll` directly, and
`Escalation` is no longer how ex commands reach `Editor`.

## Deliberately out

- **Filtering the tree.** `/` over a listing is what a file picker does better,
  and `Picker` is already built. Under this design it is a predicate over
  `rows()` whenever it earns its place.
- **Git status markers.** A column of `M` and `?` is a feature about git, not
  about project structure, and it needs a dependency and a refresh policy.
- **File watching.** `R` is the refresh. A watcher is a thread and a stream of
  events into an event loop that currently blocks on one key at a time.
- **Editing the listing to move files.** The oil.nvim model — `dd` a file, `:w`
  applies — is a real design, and it is the buffer design this spec rejected.
- **Copying, and multi-selection.** One path per operation until there is a
  reason for more.
