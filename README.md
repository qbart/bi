# bee

A batteries-included modal editor. Tree-sitter, git, and LSP are meant to be
built in, not plugins.

Status: modal editing, undo, registers, tree-sitter highlighting for Rust, a
buffer list, split windows, and a file tree.
See [RECOMMENDATION.md](RECOMMENDATION.md) for why the stack is what it is, and
[docs/specs](docs/specs) for the designs behind each piece.

```sh
cargo run -- <file>
cargo run -- .            # a directory opens the file tree
cargo test
cargo fmt --check
python3 scripts/vim_differential.py   # needs vim; not part of cargo test
```

`rustfmt.toml` sets `use_small_heuristics = "Max"`, which keeps short struct
literals, match arms and `if`/`else` on one line. That is the style the code was
already written in; the default heuristics would expand a large part of it.

## Key bindings

Counts work where vim's do: `5j`, `3dd`, `d3w`, and `2d3w` multiplies to six
words. `{n}` below means an optional count.

### Normal mode

**Motions** — usable on their own, or as the target of an operator.

| Key | Moves to |
|---|---|
| `h` `l` | left, right (`Space` also moves right) |
| `j` `k` | down, up, keeping a goal column through short lines |
| `w` `b` | start of the next / previous word |
| `f{c}` `F{c}` | onto the next / previous `{c}`, within the line |
| `t{c}` `T{c}` | just before / after it |
| `;` `,` | repeat the last find, or reverse it |
| `0` `^` | start of the line (`^` is an alias for `0` until a first-non-blank motion exists) |
| `$` | end of the line |
| `gg` | first line |
| `G` | last line, or line `{n}` when counted |
| arrows, `Home`, `End` | same as the above |

**Operators** — take a motion, or double the key for whole lines.

| Key | Does |
|---|---|
| `d{motion}` | delete over the motion: `dw` `d$` `d0` `db` `dj` `dgg` `dG` |
| `c{motion}` | change — delete, then enter insert mode |
| `y{motion}` | yank |
| `dd` `cc` `yy` | the whole line, `{n}` of them when counted |
| `D` `C` | `d$` / `c$` — to the end of the line |
| `S` | `cc` |
| `Y` | `yy` |
| `x` `s` | delete / change the char under the cursor — `dl` and `cl` |
| `X` | delete the char before the cursor — `dh` |

**Text objects** — take the thing the cursor is *inside*, rather than a
direction to move in. `i` is the object, `a` includes its surroundings.

| Key | Selects |
|---|---|
| `iw` `aw` | word; `aw` takes the whitespace after it (or before, at a line end) |
| `iW` `aW` | WORD — whitespace-delimited, so `foo.bar` is one |
| `i"` `i'` `` i` `` | the contents of the quotes; `a"` includes them and the space after |
| `i(` `i[` `i{` `i<` | inside the brackets; either bracket of a pair works, and `b`/`B` alias `(`/`{` |
| `a(` `a{` … | including the brackets |
| `ip` `ap` | paragraph — a run of non-blank lines; `ap` reaches into the blanks after |

So `diw`, `ciw`, `ci"`, `da(`, `dip` all work. When a `{ }` pair has its braces
on their own lines, `di{` is linewise and leaves them there — as in vim.

`cw` follows vim and behaves like `ce`: it changes the word without swallowing
the whitespace after it. `dw` at the end of a line stops there rather than
pulling the next line up.

**Registers and paste**

| Key | Does |
|---|---|
| `p` `P` | paste after / before the cursor, or below / above the line if the entry was taken linewise |
| `"p` `"P` | open the picker to choose from everything captured, then paste |
| `"_` | black hole prefix — `"_dd` deletes without capturing |

Every `y`, `d`, `c` and `x` captures automatically into a 4096-deep ring, so
there is nothing to decide at yank time. A count goes before the quote: `3"p`.

**Entering insert mode**

| Key | Puts the cursor |
|---|---|
| `i` `a` | before / after the cursor |
| `I` `A` | at the start / end of the line |
| `o` `O` | on a new line below / above |

**Changing text in place**

| Key | Does |
|---|---|
| `{n}r{char}` | overwrite `{n}` chars with `{char}`, refusing if the line is too short |
| `{n}~` | flip the case under the cursor and step right, stopping at the line end |
| `{n}J` | join `{n}` lines up, collapsing the indent to one space |

**Searching**

| Key | Does |
|---|---|
| `/pat` `?pat` | search forward / backward, wrapping |
| `n` `N` | repeat the search / repeat it reversed |
| `*` `#` | search forward / backward for the word under the cursor, whole-word |

A search is a motion, so `d/foo`, `c/foo` and `y/foo` all work, and it is
**exclusive** — `d/three` stops before the match. An all-lowercase pattern
matches case-insensitively; a capital anywhere in it makes the search
case-sensitive. A bare `/` repeats the last pattern. Regular expressions and
`:s` are not built yet.

Matches are **not** highlighted, which is vim's default and not the thing you
want while reading code; `:hls` turns highlighting on and `:noh` off again.

What a search leaves behind is the status line. While one is live the footer
is the search and nothing else — not even the mode block — showing the pattern
with the prefix of the direction you are travelling, so `N` after `/foo` reads
`?foo`, and `[3/17]` at the right: which match the cursor is on out of how
many. `[0/17]` means it is in front of the first.

A search stays live while the keys are still the search — `n`, `N`, another
`/`. The first key that is anything else hands the footer back, and the count
goes with it: the pattern is still remembered for `n`, but bee has stopped
counting at you.

**Scrolling**

| Key | Does |
|---|---|
| `Ctrl-E` `Ctrl-Y` | move the window one line down / up |
| `Ctrl-D` `Ctrl-U` | move it half a window down / up, taking the cursor along |

`Ctrl-E`/`Ctrl-Y` move the window and only drag the cursor when it would
otherwise fall outside. Deliberately absent: `H`/`M`/`L`, `zz`/`zt`/`zb` and
`Ctrl-F`/`Ctrl-B`, which need the viewport to be something the editor owns
rather than a row index — see [docs/specs/motions.md](docs/specs/motions.md).

**Moving lines**

| Key | Does |
|---|---|
| `Shift-Down` `Shift-Up` | move this line, or the selected block, one row — `{n}` says how far |

`:m` is vim's `:move`, and its argument is an **address** — a line to land
*after*, not a distance. `:m 0` is the top, `:m $` the bottom, `:m 12` puts the
lines after line 12, and the signed forms are addresses too: `+3` means `.+3`,
which is why `:m -1` names the line above and moves nothing, and `:m -2` is how
you go up one.

Being an address is also why it is direction-dependent: coming from above line
12 the line becomes line 12, coming from below it becomes line 13. And an
address off either end is refused rather than clamped, because a typed line
number is a claim about a line that either exists or does not.

The arrows are the other half of this, and are not vim: `Shift-Down` travels
one row, `{n}` of them, and simply stops at the end. Reach for `:m` when you
know the line, and for the arrows when you know the distance. See
[docs/specs/move-lines.md](docs/specs/move-lines.md).

In visual mode the block moves and stays selected, so nudging it is a matter of
holding the key. `m` itself is untouched, and still free for the marks the gaps
below promise.

**Repeating**

| Key | Does |
|---|---|
| `.` | repeat the last change — `{n}.` repeats it with a new count |

`.` repeats the last thing that *changed text*, so a motion, a yank, an undo or
a `:` command leaves it alone. A whole insert session is one unit, which makes
`ciwfoo` then `.` a rename. After a visual operator it repeats over the same
extent from the cursor, as vim does. With several cursors it runs at each.

**Undo**

| Key | Does |
|---|---|
| `u` | undo, `{n}` steps when counted |
| `Ctrl-R` | redo |

One command is one undo step, so `5x` returns in a single `u`. A whole insert
session is one step, including the newline `o` opened before it. Undo is a
tree, so undoing and then typing keeps the old branch rather than discarding it
— though nothing reaches those branches yet.

**Other**

| Key | Does |
|---|---|
| `v` `V` `Ctrl-V` | charwise / linewise / blockwise visual mode |
| `R` | replace mode |
| `Esc`, `Ctrl-C` | back to normal mode, or cancel a half-typed command |
| `:` | ex command line |

### Visual mode

`v` starts a charwise selection, `V` a linewise one. The same key again, or
`Esc`, leaves. Every motion and text object works — the selection's head moves
and its anchor stays put — so `viw`, `vi(`, `vap` and `v$` all do what you would
expect.

| Key | Does |
|---|---|
| `d` `x` | delete the selection |
| `c` `s` | change it |
| `y` | yank it |
| `r{char}` | overwrite every selected character |
| `o` | swap the ends, to adjust the other one |
| `iw` `i(` … | make that text object the selection |

Charwise selections include the character under the cursor, as in vim.

### Blockwise visual

`Ctrl-V` selects a rectangle. Motions move one corner and the block follows, so
`Ctrl-V` `3j` `5l` is a four-by-six block.

| Key | Does |
|---|---|
| `d` `x` `c` `s` `y` `r{char}` | as above, over the rectangle |
| `I` `A` | insert at the left / right edge of every row |
| `$` | ragged right edge — every row to its own end |
| `O` | swap the columns and keep the rows (`o` swaps corners diagonally) |
| `p` `P` | a yanked block goes back in as a rectangle |

Rows too short to reach the block are skipped rather than mangled — except by
`A`, which pads them out so what you append lines up. A block yanks as one
register entry (`▚` in the picker) that remembers it was a rectangle.

`c` and `I`/`A` leave a cursor on every row, so the text appears on all of them
as you type. Vim replicates it when you press `Esc` instead; the file ends up
the same either way.

### Multiple cursors

Vim has none of this, so the bindings are bee's own.

| Key | Does |
|---|---|
| `Ctrl-N` | add a cursor at the next occurrence of the word under the cursor, wrapping |
| `Ctrl-Alt-Down` `Ctrl-Alt-Up` | add a cursor on the line below / above, keeping the column |
| `Esc` | collapse back to one cursor |

Every command applies at every cursor, and the whole thing is one undo step —
including the cursors themselves, which come back when you undo. In visual mode
`Ctrl-N` selects the next occurrence of the *selection* instead, so
`viw` then `Ctrl-N` `Ctrl-N` then `c` is a rename.

The terminal has one real cursor; it sits on the primary selection and the
others are drawn as coloured cells. The status line shows the count, since the
mode label alone cannot tell you that more than one is live.

### Replace mode

`R` overwrites instead of inserting, until `Esc`. `Backspace` puts back what was
overwritten rather than deleting, and typing past the end of a line appends
rather than eating the newline. The whole session is one undo step.

### Insert mode

Printable keys insert themselves. `Enter`, `Backspace` and `Tab` do the obvious
thing, arrows and `Home`/`End` move, and `Esc` or `Ctrl-C` returns to normal
mode with the cursor pulled back onto a character.

### Picker

Opened with `"p` / `"P`. Typing filters by substring — every whitespace-separated
term must appear somewhere, in any order, case-insensitively. Matches stay in
ring order, so the most recent is first.

| Key | Does |
|---|---|
| any printable | add to the query |
| `Backspace` | delete a char; on an empty query, cancel |
| `Ctrl-N`, `Down` | next match |
| `Ctrl-P`, `Up` | previous match |
| `Ctrl-A` | show or hide one-character entries, hidden by default |
| `Enter` | paste the highlighted entry |
| `Esc`, `Ctrl-C` | cancel |

A `¶` beside a row means the entry is linewise and will open a new line.
Choosing an entry also moves it to the front of the ring, so a plain `p`
afterwards repeats it.

### Ex commands

| Command | Does |
|---|---|
| `:w` `:w <path>` | write |
| `:q` `:q!` | close this window; from the last one, quit — refusing unsaved changes unless forced |
| `:wa` `:qa` `:qa!` | every buffer, including ones no window is showing |
| `:wq` `:x` | write and quit |
| `:e` `:e!` | revert this buffer from disk, refusing if modified unless forced |
| `:e <path>` | open another file, reusing its buffer if it is already open |
| `:reload` | re-read the config, through the same path startup uses — different job from `:e`, which reverts the buffer |
| `:sp [path]` `:vs [path]` | split horizontally / vertically; bare, the new window duplicates this one |
| `:close` `:only` | close this window / every other one |
| `:bn` `:bp` | cycle the buffer list, wrapping |
| `:b <partial>` `:b#` | switch by path substring / to the alternate buffer |
| `:ls` `:buffers` | pick from the open buffers |
| `:bd` `:bd!` | delete this buffer, refusing unsaved changes unless forced |
| `:hls` `:noh` | start / stop highlighting every search match |
| `:set number {n}` | line numbers: `0` off, `-1` relative, `{n}` every *n*th |
| `:{n}` | go to line *n* |
| `:m 12` `:m 0` `:m $` | move this line, or the selection, after that line |
| `:m +3` `:m -2` | the same, relative to the cursor's line — vim's addresses throughout |
| `:create <path>` | an empty file, or a directory for a trailing `/`; parents are made too |
| `:rename <old> <new>` | move a file, taking any open buffer's path with it |
| `:delete <path>` `:delete!` | remove a file, or a directory `!` says may have things in it |
| `:paste [<dir>]` | put what is marked into `<dir>`, or the selected directory |
| `:paste-as <path>` | place the file a paste stopped on, and carry on |

### Windows and buffers

`Ctrl-W` starts a window command; a count in front belongs to the resize forms.
See [docs/specs/windows.md](docs/specs/windows.md).

| Key | Does |
|---|---|
| `Ctrl-W s` `Ctrl-W v` | split horizontally / vertically |
| `Ctrl-W e` | show or hide the tree beside this file, rooted at its directory with it selected |
| `Ctrl-W h j k l` | focus the window in that direction |
| `Ctrl-W w` `Ctrl-W W` | cycle focus forwards / backwards |
| `Ctrl-W c` `Ctrl-W q` | close this window |
| `Ctrl-W o` | close every other window |
| `Ctrl-W + -` `Ctrl-W < >` | taller / shorter, wider / narrower |
| `Ctrl-W =` | equalise every pane |
| `Ctrl-^` | switch to the alternate buffer (`:b#` where the terminal does not send it) |
| `Ctrl-I` `Ctrl-O` | cycle the buffer list forwards / backwards. `Ctrl-I` is `Tab`, byte for byte |

Two windows may show one buffer, with their own cursor and their own scroll.
An edit in one moves the other's cursor *with the text* rather than clamping it
— including an undo, which reaches them through the same edit log.

Closing a window discards nothing, so it never asks about unsaved changes: the
buffer stays in the list. Deleting a buffer closes no windows either — a window
showing it falls through to the next one.

### The file tree

`bee .` opens a directory, and so do `:e`, `:sp` and `:vs` — a path is a path,
and which one you meant is a question for the disk. `-` goes the other way, out
of a file and into the tree above it, and `Ctrl-W e` shows or hides one in a
pane beside it. There is only ever one tree: `-` goes to the one that is open
rather than making another. See [docs/specs/tree.md](docs/specs/tree.md).

| Key | Does |
|---|---|
| `j` `k` `gg` `G` | move, with a count |
| `l` / `→` | open a directory, or open the file under the cursor |
| `h` / `←` | close a directory, or step to the parent row |
| `Enter` | a directory toggles, a file opens |
| `-` `+` | re-root at the parent directory / at the one you are standing in |
| `gh` | show or hide dotfiles |
| `R` | re-read from disk |
| `a` `r` | create / rename — each fills in a `:` line for you to agree to |
| `dd` | delete outright. No undo for the filesystem; a directory with anything in it still wants `:delete!` |
| `y` | yank the selected path into the register ring, so `p` in a file pastes it |
| `c` `x` | mark for copying / for cutting, and unmark |
| `p` | put what is marked into the selected directory |
| `Esc` | forget what is marked |

Enter on a file opens it in the last window focused before this one, so a tree
pane is a sidebar that stays put and files land in whichever pane you reached it
from. With one window it opens in place, and `Ctrl-^` brings the tree back with
its expansion intact.

`Ctrl-W e` is the shortcut for that layout: it opens the tree beside the file
you are reading, rooted at its directory with the file already selected, and
`Chrome::tree_width` columns wide. That width is a starting point — the pane
keeps its share of the terminal from then on, like every other pane. Pressed
again, from anywhere, it puts the tree away.

`c` and `x` build one set with one mode: pressing the other key converts
everything rather than leaving a clipboard that both duplicates and destroys on
a single `p`. Marked rows carry a `+` or a `~` in a column that is there whether
anything is marked or not, so marking never shifts the tree sideways, and the
footer says `2 to copy` or `3 to move` so the mode is never something you have
to remember.

A paste never overwrites. It stops on the first clash and offers the path on the
`:` line — edit it and press Enter to place that one and carry on, or press Esc
to abandon the rest. A cut clears the marks afterwards; a copy keeps them, so
the same set can go somewhere else too.

The keymap is an allowlist, not normal mode minus the dangerous keys: anything
it does not name does nothing, which is the safe failure for a pane sitting on a
filesystem. Nothing in it enters insert mode, so a tree never can be.

### Status rows

Each window carries its own. The focused one leads with the position and ends
with the mode, so what you are typing and where you are typing it read as one
line; the rest lead with the name, dimmed, because what you want from a window
you are not typing in is what it *is*.

```
 12:5  editor.rs [+]                                            NORMAL
 main.rs  1:1
```

A tree pane has no status row at all — its own first row already names the root,
and a sidebar cannot spare a line to say so twice.

Names are file names. Which `main.rs` it is belongs to the picker, and a pane
thirty columns wide has no room to say it twice. The modified marker rides with
the name in both, because it matters most on a pane you are not looking at.

The footer underneath is the session's: messages, half-typed keys, the cursor
count, and the `:` and `/` lines.

### Line numbers

`:set number {n}` decides what the gutter shows. `:set number=5` works too,
since that is vim's spelling.

| Value | Gutter |
|---|---|
| `0` | none at all — the column is gone, not blank |
| `-1` | relative to the cursor line |
| `1` | every line, which is the default |
| `5` | every fifth line, the rest blank |

The line the cursor is on always shows its own absolute number, whatever the
mode — it is the one number a relative gutter cannot tell you, and the one
`:{n}` needs. The gutter keeps the width of the largest line number in every
mode, so moving the cursor never slides the file sideways; each window sizes
its own, since that depends on the file in the pane.

See [docs/specs/number.md](docs/specs/number.md), including why the option takes
a value rather than being a boolean, and why it is session-wide.

Vim spells this as two booleans, `number` and `relativenumber`, because a
boolean cannot say "every fifth". One option that takes a value says all three
and makes off a value like any other — so `:set nu` and `:set rnu` do not work
here, and bare `:set number` reports the current value rather than turning
numbering on.

### Config

`~/.config/bee/config.toml` — found via `$BEE_CONFIG`, which names the
*directory*, not the file, since `themes/` will need to be its sibling; else
`$XDG_CONFIG_HOME/bee`; else `~/.config/bee`. `bee config init` creates it,
writing bee's defaults commented out, and never overwrites one that already
exists; `bee config edit` opens the directory as a tree. Neither runs on its
own — a config file appears because you asked for one.

The file is a *patch* over bee's compiled-in defaults, never a replacement:
an option you never mention keeps doing what bee already does, including
whatever a later version adds. The only section today is `[options]`, which
**is** the `:set` namespace — one key per option, spelled identically, so
`:set number 5` and `number = 5` reach one setting:

```toml
[options]
number   = 5      # 0 off, -1 relative, N every Nth — see docs/specs/number.md
hlsearch = false
```

An unknown option or a value of the wrong type drops that one line and
reports it rather than refusing to start — `1 config problem: unknown
option: nmber` on the status line, not stderr, which the alternate screen
swallows. Only malformed TOML falls back wholesale, and even then bee starts,
on its defaults. `:reload` re-reads the file through the same path startup
used and swaps in whatever parsed; a failed reload changes nothing, because
reloading yourself into a config with no way to type `:reload` again is the
one outcome worth engineering against.

The theme and the keymap are specified but not built — see
[docs/specs/config.md](docs/specs/config.md).

## Layout

bee is a library plus a frontend. The library is the editor and knows nothing
about terminals; `src/tui/` is the terminal frontend, and a GUI would be its
sibling rather than a rewrite. See [docs/specs/lib-split.md](docs/specs/lib-split.md).

**The library** — `src/lib.rs`:

| File | Holds |
|---|---|
| `buffer.rs` | rope, cursor, motions, the single mutation primitive |
| `config/` | `Config`, the TOML parser and diagnostics, `ConfigSource` |
| `history.rs` | the undo tree: revisions, branching, invertible `Change`s |
| `registers.rs` | the yank ring: entries, capture, eviction |
| `editor.rs` | modes, the `Action` dispatch table, ex commands, scrolling |
| `motion.rs` | `Motion` / `Operator` / `Kind` — the vocabulary they all share |
| `picker.rs` | the overlay's state: query, matches, selection |
| `selection.rs` | selections: the editing primitive normal/visual/multi-cursor share |
| `tree.rs` | the file tree: expansion, the flattened rows, the filesystem |
| `syntax.rs` | tree-sitter: incremental reparse, highlight spans |
| `input.rs` | keys → `Command`; the `[count] op [count] motion` state machine |
| `key.rs` | `Key` / `KeyCode` / `Mods` — bee's own key vocabulary |
| `window.rs` | windows and the tree that arranges them; `Rect` layout geometry |

**The terminal frontend** — `src/main.rs`:

| File | Holds |
|---|---|
| `main.rs` | terminal lifecycle, event loop |
| `tui/render.rs` | viewport-bounded render pass |
| `tui/keys.rs` | crossterm key events → `bee::key::Key` |

## The seven decisions this step locks in

1. **Cursor is a char index.** Byte and UTF-16 conversions happen at the edges
   (`Buffer::point_at`), never inside motion code. LSP wants UTF-16 columns,
   tree-sitter wants byte columns; neither leaks inward.
2. **One mutation primitive.** Every edit goes through `Buffer::apply_edit`,
   which records an `Edit` carrying old/new byte ranges and points — exactly
   `tree_sitter::InputEdit`. The rope field is private to enforce this.
   `Editor::sync_syntax` drains `pending_edits` each frame — tree-sitter reads
   it today, and LSP `didChange` will read the same drain.

   `apply_edit` is a thin wrapper over `edit_raw`: the raw form mutates the rope
   and logs the `Edit`, the wrapper additionally records undo history. Undo and
   redo are the only callers of the raw form, so a new editing method gets undo
   by construction — and because history replays *through* `edit_raw`, an undo
   reaches tree-sitter and LSP as an ordinary incremental edit rather than a
   reason to reparse the file.
3. **Motions are data, not actions.** A [`Motion`] describes a destination;
   `Action::Move` goes there and `Action::Operate` deletes to there. Both
   resolve it through the same pure `fn(&self, Cursor) -> Cursor` functions on
   `Buffer`, so `w` and `dw` cannot drift apart. The cursor is a `Cursor` value
   rather than a field for the same reason an operator needs to ask where a
   motion *would* land without going there — and it is the shape visual mode
   (anchor + head) and split windows (one per view) needed — and both now use.
4. **Registers are a ring, not named slots.** Vim makes you pick one of 36
   slots at yank time, which is the wrong moment — you rarely know yet whether
   a thing is worth keeping. Every `y`/`d`/`c`/`x` captures automatically into
   a 4096-deep ring, and the choice moves to paste time where a picker can
   search it. The ring lives on `Editor`, not `Buffer`: yanking in one file and
   pasting in another is the point.
5. **Undo is a tree, not a stack.** Undoing and then typing adds a second child
   to the current revision instead of discarding the first, so no keystroke can
   make earlier work unreachable. `u` / `Ctrl-R` walk one branch; the graph
   already stores what `g-` / `g+` would later traverse chronologically.

   Grouping happens at the command boundary in `Editor::apply`, not per
   mutation — `5x` is one undo step. Insert mode holds the group open until
   `Esc`, so a typing run undoes in one go, along with the `\n` that `o`
   inserted before it.
6. **Highlighting emits capture names, not styles.** `syntax.rs` produces
   `keyword` / `string` / `comment`; `tui/render.rs` maps those to colours. A GUI
   frontend writes its own table, and a theme file eventually replaces both.
   Producing `ratatui::Style` in the core would weld it to the terminal in the
   one place that is hardest to unpick.
7. **Rendering is viewport-bounded.** Frame cost scales with terminal height,
   not buffer size — including highlighting, which queries only the visible
   byte range.

## Known gaps

- Undo groups don't break on cursor movement inside insert mode; vim starts a
  new group when you arrow away mid-insert.
- No `g-` / `g+` / `:earlier`, so abandoned branches are stored but unreachable.
- Undo history is per-session; vim persists it with `'undofile'`.
- Display width counts chars, so CJK and combining chars misalign the cursor.
  Needs `unicode-width` and a grapheme walk.
- No horizontal scrolling — long lines clip.
- No moving a window around the tree (`Ctrl-W x` / `r` / `H J K L`), and no tab
  pages. The split tree is the shape that would carry both.
- No named registers (`"n`) and no system clipboard (`"+` / `"*`). See
  `docs/specs/registers.md`.
- Rust is the only grammar. Adding one is a line in `syntax.rs`, but each is a
  C library that costs build time.
- No tree-sitter injections, so code fences in markdown and JSX would not
  highlight. No indent queries, so no auto-indent. See
  `docs/specs/tree-sitter.md`.
- No regular expressions in search, and no `:s`. See
  [docs/specs/search.md](docs/specs/search.md).
- No marks (`m{a}`, `` `{a} ``), and no `gn`.
- No git and no LSP, though both are the point — see
  [RECOMMENDATION.md](RECOMMENDATION.md). LSP hangs off `Editor::sync_syntax`,
  the same edit drain tree-sitter uses.

### Architectural, and cheaper to fix now than later

- **Tree-sitter is not optional, so building needs a C toolchain.** Grammars
  are C compiled by `cc`, which breaks minimal containers and makes
  cross-compilation harder. A Cargo feature would fix it: `Editor::syntax` is
  already `Option<Syntax>` and an unknown extension already renders as plain
  text, so the no-syntax path exists and works.
- ~~**The config language is undecided**~~ Decided and half built: TOML, in
  `~/.config/bee/config.toml`, parsed by `bee::config`. `[options]` is live.
  The keymap in `input.rs` and the highlight table in `tui/render.rs` are still
  hardcoded and are steps 3 and 2 of [docs/specs/config.md](docs/specs/config.md).
- **The core/frontend boundary is enforced by a test, not the compiler.** Fixed
  as far as it goes: there is a `lib.rs`, `input.rs` speaks `bee::key::Key`
  rather than crossterm's types, and rendering and event translation live in
  `src/tui/`. But a lib and a bin in one package share one dependency list, so
  nothing stops `editor.rs` from importing ratatui — except
  `tests/lib_boundary.rs`, which reads the library's modules and fails on any
  that name a terminal crate. Only a Cargo workspace would make that a compiler
  rule, and that is not worth its churn while there is one frontend.
- ~~**The cursor lives on `Buffer`.**~~ Fixed: `Editor` owns a `Selections` set
  and `Buffer` is a text store with no notion of where anyone is looking. Normal
  mode is every selection collapsed, visual is one with room in it, multi-cursor
  is several. See [docs/specs/selections.md](docs/specs/selections.md).
- **`Editor::scroll` is a row index** and `scroll_to_cursor` takes a height in
  rows, which bakes in "the viewport is N whole lines". Soft wrap breaks that
  assumption, and so does any pixel-scrolling frontend.

## Next

The rest of [docs/specs/config.md](docs/specs/config.md): the theme (step 2),
so `tui/render.rs` stops hardcoding colours, and the keymap (step 3), so
`input.rs` stops hardcoding keys. Then LSP, which hangs off the same
`pending_edits` drain that tree-sitter and the window fixup now share —
`Editor::settle` is where `textDocument/didChange` goes.

Previously, on windows: `Editor` became a session holding a buffer list and a
tree of windows, with the editing commands moved onto a `View` that binds one
buffer and one window for the length of a command. Deliberately *not* yet: a
config language or a plugin system.
