# bi

A batteries-included modal editor. Tree-sitter, git, and LSP are meant to be
built in, not plugins.

Status: modal editing, undo, registers, tree-sitter highlighting for twenty
languages, a buffer list, split windows, and a file tree.
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
| `e` `ge` | end of the next / previous word |
| `W` `B` `E` `gE` | the same, over WORDs — whitespace-delimited, so `foo.bar` is one |
| `f{c}` `F{c}` | onto the next / previous `{c}`, within the line |
| `t{c}` `T{c}` | just before / after it |
| `;` `,` | repeat the last find, or reverse it |
| `0` | column zero |
| `^` `g_` | first / last non-blank of the line |
| `$` | end of the line |
| `%` | the bracket matching the one at or after the cursor |
| `{` `}` | previous / next blank line — the paragraph boundary |
| `gg` | first line |
| `G` | last line, or line `{n}` when counted |
| arrows, `Home`, `End` | same as the above |

`e` is *inclusive* and `w` is *exclusive*, which is why both exist: `de` takes
the word, `dw` takes the word and the space after it.

**Operators** — take a motion, or double the key for whole lines.

| Key | Does |
|---|---|
| `d{motion}` | delete over the motion: `dw` `d$` `d0` `db` `dj` `dgg` `dG` |
| `c{motion}` | change — delete, then enter insert mode |
| `y{motion}` | yank |
| `>{motion}` `<{motion}` | indent / outdent the lines it covers |
| `dd` `cc` `yy` | the whole line, `{n}` of them when counted |
| `>>` `<<` | the whole line, `{n}` of them when counted |
| `D` `C` | `d$` / `c$` — to the end of the line |
| `S` | `cc` |
| `Y` | `yy` |
| `x` `s` | delete / change the char under the cursor — `dl` and `cl` |
| `X` | delete the char before the cursor — `dh` |

**Jumping** — `s` dims the screen the moment you press it, then every match of
what you type gets a letter after it; press the letter and you are on the first
character of that match. The letters are inserted between the cells rather than
drawn over them, so nothing on the line is hidden by the thing pointing at it.
The letters never include a character that could narrow the search
further, so typing and jumping share the keyboard with no mode switch between
them. Only the viewport is searched, `Esc` leaves, `Backspace` takes a
character back, and a query that matches nothing leaves by itself. Vim's `s`
was `cl` spelled shorter, and `cl` still works. See
[docs/specs/find.md](docs/specs/find.md).

**Selecting by structure** — `S` puts a letter at *both ends* of every scope
around the cursor, tightest first: `a` inside `b` inside `c`, so one letter
tells you where a scope starts and where it ends before you commit to it. The
letters go *between* the characters rather than over them, and two scopes
ending in the same place get a cell each — `}ab` — so nothing on the line and
nothing in the list is lost to a letter.
Press one and it becomes the selection. The list is the chain of tree-sitter
nodes containing the cursor, so `{ "hello/plugin" }` in Lua offers the string's
contents, the string, and the table with no special case for any of them. Vim's
`S` was `cc` spelled shorter, and `cc` still works. See
[docs/specs/scopes.md](docs/specs/scopes.md).

**Surroundings** — add, remove and change what is around something.

| Key | Does |
|---|---|
| `ys{motion}{char}` | wrap what the motion covers — `ysiw"`, `ys2w)`, `ysip>` |
| `yss{char}` | wrap the whole line |
| `ds{char}` | delete the innermost pair around the cursor |
| `cs{old}{new}` | change one pair into another, in place |

`(` `{` `[` `<` put a space inside — `{ x }` — and `)` `}` `]` `>` `b` `B` do
not. Any of them *finds* the same pair, so `ds(`, `ds)` and `dsb` all delete
the nearest parentheses. Every one of these keeps the cursor where it is, which
is what makes `cs"'` from inside a string worth having. `S{char}` in visual
mode wraps the selection. Tags (`dst`) are not here — a tag is a parse rather
than a pair; see [docs/specs/surround.md](docs/specs/surround.md).

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
| `p` `P` | paste after / before the cursor, or below / above the line if the entry was taken linewise — over a selection they replace it |
| `"p` `"P` | open the picker to choose from everything captured, then paste |
| `"_` | black hole prefix — `"_dd` deletes without capturing |
| `"+y` `"+p` | the system clipboard; `"*` is a spelling of the same register |

Every `y`, `d`, `c` and `x` captures automatically into a 4096-deep ring, so
there is nothing to decide at yank time. A count goes before the quote: `3"p`.

The system clipboard is explicit rather than mirrored: a delete is not a copy,
and exporting every `dd` to the desktop is a surprise in the direction that
cannot be undone. It travels by OSC 52, so it works over SSH — but reading it
back is something many terminals refuse on purpose, and `"+p` says so when one
does. Pasting *into* bi with the terminal's own paste needs none of that: it
arrives whole, as one undo step.

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
goes with it: the pattern is still remembered for `n`, but bi has stopped
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
you go up one. Write the `.` out if your fingers do — `:m .+1` and `:m .-2` are
the same two addresses, and so are `:m+1` and `:m-2` with nothing in between.

A range in front of the command says *which* lines: `:2,3m 4`, `:%m 0`,
`:'<,'>m $`. With none written the selection does. Ranges are their own small
language — `%`, `.`, `$`, a line number, `'<` and `'>`, each with `+N`/`-N`
offsets — shared by every command that takes one; see
[docs/specs/ranges.md](docs/specs/ranges.md). A range with no command after it
goes to its last line, which is what `:42` has always been.

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
| `p` `P` | replace it with the register |
| `r{char}` | overwrite every selected character |
| `>` `<` | indent / outdent the selected lines, `{n}` steps |
| `o` | swap the ends, to adjust the other one |
| `iw` `i(` … | make that text object the selection |

Charwise selections include the character under the cursor, as in vim.

`p` and `P` differ only in what happens to the text they displaced: `p` puts it
on the ring, so select-`p` swaps two things, and `P` leaves the ring alone, so
the same entry can be pasted over one selection after another. A linewise
entry pasted over part of a line splits the line and lands between the halves,
which is vim's rule and the reason the kinds are worth keeping apart.

### Blockwise visual

`Ctrl-V` selects a rectangle. Motions move one corner and the block follows, so
`Ctrl-V` `3j` `5l` is a four-by-six block.

| Key | Does |
|---|---|
| `d` `x` `c` `s` `y` `r{char}` | as above, over the rectangle |
| `I` `A` | insert at the left / right edge of every row |
| `$` | ragged right edge — every row to its own end |
| `O` | swap the columns and keep the rows (`o` swaps corners diagonally) |
| `p` `P` | replace the rectangle — a charwise entry lands on every row |

Rows too short to reach the block are skipped rather than mangled — except by
`A`, which pads them out so what you append lines up. A block yanks as one
register entry (`▚` in the picker) that remembers it was a rectangle.

`c` and `I`/`A` leave a cursor on every row, so the text appears on all of them
as you type. Vim replicates it when you press `Esc` instead; the file ends up
the same either way.

### Multiple cursors

Vim has none of this, so the bindings are bi's own.

| Key | Does |
|---|---|
| `Ctrl-N` | add a cursor at the next occurrence of the word under the cursor, wrapping |
| `Ctrl-X` | skip this occurrence — move the newest cursor on to the next one instead |
| `Ctrl-Alt-Down` `Ctrl-Alt-Up` | add a cursor on the line below / above, keeping the column |
| `Esc` | collapse back to one cursor |

Every command applies at every cursor, and the whole thing is one undo step —
including the cursors themselves, which come back when you undo. In visual mode
`Ctrl-N` selects the next occurrence of the *selection* instead, so
`viw` then `Ctrl-N` `Ctrl-N` then `c` is a rename.

`Ctrl-X` is the same search as `Ctrl-N` with the opposite answer: it moves the
cursor you just placed rather than leaving one behind, so a match you do not
want to edit is stepped over. `Ctrl-N` `Ctrl-X` `Ctrl-N` takes the first and
third occurrences and leaves the second alone. Neither key does anything in
blockwise visual, where the rectangle comes from one selection's corners.

The terminal has one real cursor; it sits on the primary selection and the
others are drawn as coloured cells. The status line shows the count, since the
mode label alone cannot tell you that more than one is live.

### Replace mode

`R` overwrites instead of inserting, until `Esc`. `Backspace` puts back what was
overwritten rather than deleting, and typing past the end of a line appends
rather than eating the newline. The whole session is one undo step.

### Insert mode

Printable keys insert themselves, arrows and `Home`/`End` move, and `Esc` or
`Ctrl-C` returns to normal mode with the cursor pulled back onto a character.

`Tab` moves to the next indent stop rather than inserting a fixed width, so a
line lines up with the one above it whatever column you started from;
`Shift-Tab` goes back one, and never eats a character you typed. `Backspace`
takes a whole indent when there is nothing but whitespace to its left, and one
character everywhere else. `Enter` starts the new line under the old one, with
the same indent characters — a tab-indented file stays tab-indented. A line you
opened and thought better of loses its indent again on `Esc`, rather than
leaving whitespace nothing can see. See
[docs/specs/indent.md](docs/specs/indent.md).

### Picker

One overlay over five lists: the register ring (`"p` / `"P`), the open buffers
(`:ls`), every file under the session's root (`Ctrl-P`), a tree pane's paths
(`gf` and `/` in one), and the `:` lines you have run (`Ctrl-R` on the command
line). Typing filters by substring — every whitespace-separated term must
appear somewhere, in any order, case-insensitively. Matches keep the order they
were given, so the most recent is first — which is an answer to "which one did
you mean" that only the tree list has no version of, and so the only one that
is *ranked*: consecutive characters beat characters at a path or word boundary,
which beat characters that merely counted, and a shorter path breaks a tie.

The path lists are the exception: they match a *subsequence*, so
`sfr` finds
`src/find/render.rs`. Over prose that rule would match everything, which is why
it is not the default; over paths it is the only useful one. The walk skips
hidden entries and everything the project ignores — its `.gitignore` files
from the repository root down, and its `.git/info/exclude` — and stops at
20,000 files, saying so when it does. `:set gitignore false` lists everything
again; nothing else consults it, and `:e` on an ignored path has always
worked. bi's walk is checked against git's own in
`tests/gitignore_git.rs`. See [docs/specs/files.md](docs/specs/files.md) and
[docs/specs/gitignore.md](docs/specs/gitignore.md).

The buffer list is ordered by when each buffer was last *shown* and opens on
the second row — the one you were in before this one — so `Ctrl-Tab` `Enter`
switches back and doing it twice returns you. Where a terminal cannot tell
`Ctrl-Tab` from `Tab` you simply get buffer-next, which is what that key always
did — and `gf` opens the switcher on every terminal, beside `ga` for the
alternate file. See [docs/specs/buffers.md](docs/specs/buffers.md).

Only the register ring gets a preview pane. A file name, a command line and a
buffer are each already the row you are reading, so the pane would repeat it
and take a third of the overlay to do it; the rows get the space instead.

| Key | Does |
|---|---|
| any printable | add to the query |
| `Backspace` | delete a char; on an empty query, cancel |
| `Ctrl-N`, `Down` | next match |
| `Ctrl-P`, `Up` | previous match |
| `Ctrl-A` | show or hide one-character entries, hidden in the register list only |
| `Enter` | take the highlighted row |
| `Esc`, `Ctrl-C` | cancel, back to wherever it was opened from |

A `¶` beside a row means the entry is linewise and will open a new line.
Choosing a register also moves it to the front of the ring, so a plain `p`
afterwards repeats it.

### Command line

`:` opens it. Every printable key is text, so nothing there is a command.

| Key | Does |
|---|---|
| `Ctrl-R` | the picker over the `:` lines you have run, with what you have typed as the query |
| `Enter` | run it |
| `Backspace` | delete a char; on an empty line, leave |
| `Esc`, `Ctrl-C` | cancel |

A chosen history line is **put back on the `:` line and not run**, so the command
with one word wrong is one keystroke from being fixed. The history is the
session's — it is not written to disk — holds 200 lines, keeps one copy of a
line however often you run it, and records what you typed rather than what a
keybinding ran. See [docs/specs/cmdline-history.md](docs/specs/cmdline-history.md).

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
| `:sp [path]` `:vs [path]` | split below / right; bare, the new window duplicates this one |
| `:new` `:vnew` | split onto a new unnamed buffer, rather than a second view of this one |
| `:enew` | an unnamed buffer in this window |
| `:close` `:only` | close this window / every other one |
| `:bn` `:bp` | cycle the buffer list, wrapping |
| `:b <partial>` `:b#` | switch by path substring / to the alternate buffer |
| `:ls` `:buffers` | the buffer switcher — same list, every terminal |
| `:bd` `:bd!` | delete this buffer and close the windows showing it, refusing unsaved changes unless forced |
| `:hls` `:noh` | start / stop highlighting every search match |
| `:set number {n}` | line numbers: `0` off, `-1` relative, `{n}` every *n*th |
| `:{n}` `:$` `:%` | a range and no command goes to its last line |
| `:m 12` `:m 0` `:m $` | move this line, or the selection, after that line |
| `:m +3` `:m -2` | the same, relative to the cursor's line — vim's addresses throughout |
| `:m .+1` `:m+1` | the `.` written out, or the space left off. Both are the address above |
| `:2,5m 0` `:%m $` | a range says *which* lines; with none, the selection does |
| `:alt` | the other file — the test beside the implementation, the header beside the source |
| `:case <style>` | respell the selection, or the word under the cursor |
| `:create <path>` | an empty file, or a directory for a trailing `/`; parents are made too |
| `:rename <old> <new>` | move a file, taking any open buffer's path with it |
| `:delete <path>` `:delete!` | remove a file, or a directory `!` says may have things in it |
| `:paste [<dir>]` | put what is marked into `<dir>`, or the selected directory |
| `:paste-as <path>` | place the file a paste stopped on, and carry on |

`:alt` is `ga`, and walks `[alternate]` in order: the first pattern that
matches your path decides, then the first of its paths that exists is opened.
`*` matches anything, separators included, and stands for the same text on the
right. Go, C and C++ pairs are built in; a pattern you set replaces bi's.
`<leader>a` is a line of config — `"<leader>a" = ":alt<CR>"` — rather than a
default, because bi's leader has no built-in meaning and the first binding to
claim one should be yours. See
[docs/specs/alternate.md](docs/specs/alternate.md).

`:case` takes `upper`, `lower`, `title`, `camel`, `pascal`, `snake`, `dash` or
`const` — one name each, no aliases.
It respells every *identifier* in range and leaves what is between them
alone, so `foo_bar baz_qux` in camel is `fooBar bazQux`. With nothing selected
it takes the word under the cursor, which is what renaming one usually is. See
[docs/specs/case.md](docs/specs/case.md).

### Windows and buffers

`Ctrl-W` starts a window command; a count in front belongs to the resize forms.
See [docs/specs/windows.md](docs/specs/windows.md).

| Key | Does |
|---|---|
| `Ctrl-W s` `Ctrl-W v` | split horizontally / vertically |
| `Ctrl-W e` | show or hide the tree beside this file, rooted where the session is with the file revealed |
| `Ctrl-W h j k l` | focus the window in that direction |
| `Ctrl-W f` | a letter in the middle of every window — press one to go there |
| `Ctrl-W w` `Ctrl-W W` | cycle focus forwards / backwards |
| `Ctrl-W c` `Ctrl-W q` | close this window |
| `Ctrl-W o` | close every other window |
| `Ctrl-W + -` `Ctrl-W < >` | taller / shorter, wider / narrower |
| `Ctrl-W =` | equalise every pane |
| `Ctrl-^` | switch to the alternate buffer (`:b#` where the terminal does not send it) |
| `Ctrl-I` `Ctrl-O` | cycle the buffer list forwards / backwards. `Ctrl-I` is `Tab`, byte for byte |
| `Ctrl-Tab` `gf` | the buffer switcher: newest first, opening on the one you were in before |

Two windows may show one buffer, with their own cursor and their own scroll.
An edit in one moves the other's cursor *with the text* rather than clamping it
— including an undo, which reaches them through the same edit log.

Closing a window discards nothing, so it never asks about unsaved changes: the
buffer stays in the list. Deleting one is the other way round — every window
showing it closes, and the layout collapses to fill the space, so what is left
on screen is what you did not delete. The window you are in is the one that
survives if they all showed it; there is always a window, and it falls through
to the next buffer.

### The file tree

`bi .` opens a directory, and so do `:e`, `:sp` and `:vs` — a path is a path,
and which one you meant is a question for the disk. `-` goes the other way, out
of a file and back into the tree, and `Ctrl-W e` shows or hides one in a pane
beside it. There is only ever one tree: `-` goes to the one that is open rather
than making another.

**The root is the session's.** Whatever directory you opened is where every
tree opens, with the file you were in revealed; opening a file never moves it.
Only naming a directory, `+` and `-` do. See
[docs/specs/tree.md](docs/specs/tree.md).

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
| `Ctrl-P` | the file picker — a tree is where you look files up |
| `gf` | find any path under the root by name and go to it, opening the way down. Rows already on screen win a tie and lose to a better match |
| `/` | the same list, narrowed to the rows on screen |

Enter on a file opens it in the last window focused before this one, so a tree
pane is a sidebar that stays put and files land in whichever pane you reached it
from. `Ctrl-P` and `:e <file>` follow the same rule, so nothing opened from a
tree closes the tree. With one window it opens in place, and `Ctrl-^` brings the
tree back with its expansion intact.

A split opens on the far side — below for `:sp`, right for `:vs` — and focus
goes there, so you can see that you moved. Vim's default is the other way
round, which made focus look broken: the new pane took the space the old one
occupied. The tree sidebar is the exception and still opens on the left.

`Ctrl-W f` puts a letter in the middle of every window — home row first, `f`
and `j` before the rest — and the next key goes there. A three-row block, not a
single character in a corner: it names a whole pane, and on a screen of four
panes of code one more character is a thing you have to hunt for. It is the first client
of bi's label machinery, which `s` and `S` will reuse; see
[docs/specs/labels.md](docs/specs/labels.md). Not `<Tab>`, which is `Ctrl-I`
byte for byte in a terminal and would have taken buffer-next with it.

`Ctrl-W e` is the shortcut for that layout: it opens the tree beside the file
you are reading, rooted at its directory with the file already selected, and
`Chrome::tree_width` columns wide. That width is a starting point — the pane
keeps its share of the terminal from then on, like every other pane. Pressed
again, from anywhere, it puts the tree away.

`c` and `x` mark **per path**, so one `p` can copy some files and move others.
Pressing the other key on an already-marked path converts that one rather than
unmarking it — "make this a move" is what you meant — and the key that put a
mark there is the key that takes it away. Marked rows carry a `+` or a `~` in a
column that is there whether anything is marked or not, so marking never shifts
the tree sideways, and the footer says `1 to copy, 2 to move`. That column is
what makes mixing safe to offer: every row shows which it is, all the time.

A paste never overwrites. It stops on the first clash and offers the path on the
`:` line — edit it and press Enter to place that one and carry on, or press Esc
to abandon the rest. Afterwards the cut marks are gone — their sources are not
there any more — and the copy marks stay, so the same files can go somewhere
else too.

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

`~/.config/bi/config.toml` — found via `$BI_CONFIG`, which names the
*directory*, not the file, since `themes/` will need to be its sibling; else
`$XDG_CONFIG_HOME/bi`; else `~/.config/bi`. `bi config init` creates it,
writing bi's defaults commented out, and never overwrites one that already
exists; `bi config edit` opens the directory as a tree. Neither runs on its
own — a config file appears because you asked for one.

The file is a *patch* over bi's compiled-in defaults, never a replacement:
an option you never mention keeps doing what bi already does, including
whatever a later version adds. `[options]` **is** the `:set` namespace — one key per option, spelled identically, so
`:set number 5` and `number = 5` reach one setting:

```toml
[options]
number     = 5      # 0 off, -1 relative, N every Nth — see docs/specs/number.md
hlsearch   = false
tab_width  = 4      # how wide a \t is drawn
shiftwidth = 0      # how far `>` moves; 0 means "the same as tab_width"
expandtab  = true   # write an indent as spaces
autoindent = true   # a new line starts under the one above it
```

Options resolve **per file**, not per session. `[options]` is what you want in
general; bi has a small built-in table of what a language *requires* — a
Makefile gets tabs, Go gets tabs — and `[filetype.<name>]` is where you
override either:

```toml
[filetype.go]
tab_width = 4      # gofmt writes tabs; how wide they look is yours
```

The name is the one bi gives the file, which is the same name its grammar is
chosen by. The order is: bi's defaults, then `[options]`, then the file's type,
then the project's `.editorconfig`, then anything you `:set` this session — so
a Makefile keeps its tabs however you like your spaces, and `:set` still wins
when you mean it. See [docs/specs/options.md](docs/specs/options.md).

#### Indent guides

A vertical line down each level of indentation, at every step and never on the
text itself. A blank line keeps the guides of the block it is inside and drops
them where a block ends. `:set indent_guides false` turns them off; the colour
is the theme's `indent_guide`.

They are the first client of bi's decoration layer — the core answers "what is
drawn here that is not buffer text" and the frontend paints it, which is what
`TODO:` tags, colour swatches and jump labels will all arrive through. See
[docs/specs/decorations.md](docs/specs/decorations.md).

#### `TODO:` and friends

`TODO:`, `FIX:`, `HACK:`, `WARN:`, `PERF:`, `NOTE:` and `TEST:` are picked out
of the text in five colours the theme names — `FIXME`, `BUG`, `XXX`, `OPTIM`
and the rest are aliases of those five, because the same thought has more than
one spelling. Uppercase, on a word boundary, colon required, `TODO(name):`
included. Anywhere in the file rather than in comments only: a `TODO:` in a
Markdown list or a YAML file is exactly where people write them, and bi does
not have a grammar for every file. `:set todo_comments false` turns it off. See
[docs/specs/todo-comments.md](docs/specs/todo-comments.md).

#### A flash on what was yanked

`yy` prints nothing, changes nothing and moves nothing — it is the one command
whose whole effect is invisible. So what it read lights up for `yank_flash`
milliseconds (150 by default, `0` turns it off), in the theme's `flash`.
Charwise, linewise, blockwise — a rectangle lights one span per row rather than
its bounding box. Nothing else flashes: a delete or a paste is already visible
in the text. See [docs/specs/flash.md](docs/specs/flash.md).

#### Colour swatches

`#fb4934`, `#f94`, `rgb(251,73,52)`, `rgba(...)` and the shader spelling
`rgb(0.5f,0.1f,0.1)` are drawn in the colour they name, with black or white
text over them — whichever can actually be read, by WCAG luminance rather than
a brightness average, which is the rule that gets saturated green right. `:set
color_swatches false` turns it off. See
[docs/specs/colors.md](docs/specs/colors.md).

#### Trimming

`:w` tidies the file on its way out: trailing whitespace goes, the blank lines
at either end of it go, and a file that does not end in a newline gets one. All
five are on, because a write that tidies is the point; a project that disagrees
says so once, in its config or its `.editorconfig`, and is obeyed everywhere.

```toml
[options]
trim_on_write      = true    # the master switch
trim_trailing      = true
trim_first_line    = true
trim_last_line     = true
trim_final_newline = true
```

Markdown keeps its trailing spaces, because two of them are a hard line break
there — a built-in `[filetype.markdown]` default rather than a blocklist, so
markdown still gets everything that would not break it. The trim is its own
undo step, so `u` after a `:w` puts the whitespace back and nothing else, and
the cursor follows the text rather than the line number. See
[docs/specs/trim.md](docs/specs/trim.md).

#### `.editorconfig`

Read, with no switch to turn it on and none to turn it off: a repository that
has one means it. `root`, `indent_style`, `indent_size`, `tab_width`,
`trim_trailing_whitespace` and `insert_final_newline` become options; `charset` and `end_of_line` are ignored because bi is always UTF-8 and
always writes `\n`; the rest are for editors with features bi does not have
yet. The nearest file to yours wins, `root = true` stops the walk, and the glob
dialect is the format's own — `**`, `{a,b}`, `{1..9}` and all. Nothing is
cached, so `:reload` picks up an edit to it. See
[docs/specs/editorconfig.md](docs/specs/editorconfig.md).

An unknown option or a value of the wrong type drops that one line and
reports it rather than refusing to start — `1 config problem: unknown
option: nmber` on the status line, not stderr, which the alternate screen
swallows. Only malformed TOML falls back wholesale, and even then bi starts,
on its defaults. `:reload` re-reads the file through the same path startup
used and swaps in whatever parsed; a failed reload changes nothing, because
reloading yourself into a config with no way to type `:reload` again is the
one outcome worth engineering against.

#### Keys

`[keys.normal]`, `[keys.visual]` and `[keys.tree]` rebind keys. A binding names
a command; `false` unbinds. Either side may be more than one key, and
`<leader>` stands for whatever `[keys] leader` says — `<Space>` unless you
change it:

```toml
[keys]
leader = " "

[keys.normal]
"h" = false            # unbound
"j" = "left"           # hjkl shifted one key right
"k" = "down"
"l" = "up"
";" = "right"
"<leader>e" = "window_tree"
"<leader>f" = "window_pick"    # what `Ctrl-W f` is called
"<leader>t" = "goto_first_line"

[keys.tree]
"k" = "tree_select_down"
";" = "tree_expand"
"<leader>d" = "tree_delete"
```

Rebinding a motion rebinds every use of it: with the above, `d2k` deletes two
lines down and `v` `k` extends a selection down, because the key is rewritten
before the grammar sees it. `[keys.visual]` is a *narrower* map, not a
replacement — visual falls back to `[keys.normal]`, matching how visual mode
already falls through to normal for anything it does not claim. Nothing is
remapped while you are typing text: insert, replace, the command line, the
search line and the picker all take keys literally.

**A binding can be a `:` line.** A value starting with `:` is a command rather
than a name, which makes everything `:` can do bindable — and none of it was
reachable before, because a name has to be something bi already has keys for:

```toml
[keys.normal]
"<leader>d" = ":bd<CR>"          # runs it
"<leader>n" = ":set number 0<CR>"
"<leader>e" = ":e "              # prefills it, and waits for the path
```

The `<CR>` is what runs the line. Leave it off and the line is **prefilled** on
the command line for you to finish — which is how you bind a command that takes
an argument, and is the same trick the tree's `a` and `r` keys use. A bare `":"`
just opens the command line. The count never repeats either form: `3<leader>d`
deletes one buffer.

An unknown command name is reported with a suggestion — `unknown command:
tree_expnd — did you mean tree_expand?` — on the same status line as any other
config problem.

**A prefix has no meaning of its own.** The moment a binding spells
`<leader>e`, `<Space>` stops being "move right": there is no timeout to decide
between the two, which is deliberate — it is the reason vim pauses before `j`
moves. Type `<Space>` and something unbound and the `<Space>` is dropped, with
the other key still doing its job. Half-typed sequences show in the footer
beside the count. The same rule is why binding `"gd"` is reported: it takes `g`
over, and `gg`, `ge`, `gE` and `g_` stop resolving until you bind them back by
name.

Keys a command is already waiting for are never remapped, so `r<Space>` still
writes a space and `f<Space>` still finds one.

**What you cannot bind yet.** A name has to be something bi already has keys
for, so a command with no key at all — `git_blame` — is not bindable, and it is
reported at load rather than ignored. The theme is specified but not built, and
so is the last piece of the keymap design: a binding that resolves to a command
rather than to bi's own keys. See
[docs/specs/config.md](docs/specs/config.md).

## Layout

bi is a library plus a frontend. The library is the editor and knows nothing
about terminals; `src/tui/` is the terminal frontend, and a GUI would be its
sibling rather than a rewrite. See [docs/specs/lib-split.md](docs/specs/lib-split.md).

**The library** — `src/lib.rs`:

| File | Holds |
|---|---|
| `buffer.rs` | rope, cursor, motions, the single mutation primitive |
| `config/` | `Config`, the TOML parser and diagnostics, `ConfigSource` |
| `history.rs` | the undo tree: revisions, branching, invertible `Change`s |
| `cmd_history.rs` | the `:` lines you have run, newest first |
| `registers.rs` | the yank ring: entries, capture, eviction |
| `editor.rs` | modes, the `Action` dispatch table, ex commands, scrolling |
| `motion.rs` | `Motion` / `Operator` / `Kind` — the vocabulary they all share |
| `picker.rs` | the overlay's state: query, matches, selection |
| `range.rs` | `:` line addresses — `%`, `.`, `$`, `'<`, offsets, and resolving them |
| `selection.rs` | selections: the editing primitive normal/visual/multi-cursor share |
| `tree.rs` | the file tree: expansion, the flattened rows, the filesystem |
| `syntax.rs` | tree-sitter: incremental reparse, highlight spans |
| `input.rs` | keys → `Command`; the `[count] op [count] motion` state machine |
| `key.rs` | `Key` / `KeyCode` / `Mods` — bi's own key vocabulary |
| `window.rs` | windows and the tree that arranges them; `Rect` layout geometry |

**The terminal frontend** — `src/main.rs`:

| File | Holds |
|---|---|
| `main.rs` | terminal lifecycle, event loop |
| `tui/render.rs` | viewport-bounded render pass |
| `tui/keys.rs` | crossterm key events → `bi::key::Key` |

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
- No named registers (`"n`). The system clipboard is in — `"+y` and `"+p`, with
  `"*` as a spelling of the same — over OSC 52, so it works over SSH. See
  [docs/specs/clipboard.md](docs/specs/clipboard.md) and
  `docs/specs/registers.md`.
- Twenty grammars: Rust, C, C++, C3, Go, Python, Lua, Bash, CSS, GLSL, HLSL,
  Slang, HCL/Terraform, Dockerfile, CMake, TOML, YAML, JSON, INI and Markdown.
  Adding one is a line in `syntax.rs`, but each is a C library that costs build
  time and binary size, and three of them ship no highlight query — see
  `docs/specs/tree-sitter.md`.
- No tree-sitter injections, so a code fence in markdown highlights as a fence
  and its contents stay plain — and markdown's own inline syntax (`**bold**`,
  links, code spans) is a second grammar reached the same way, so it is
  unhighlighted too. No indent queries either, so `o` and `Enter` copy the
  indent of the line above rather than working out what the language wants,
  and there is no `=`. See `docs/specs/tree-sitter.md`.
- No regular expressions in search, and no `:s`. See
  [docs/specs/search.md](docs/specs/search.md).
- No marks (`m{a}`, `` `{a} ``), and no `gn`.
- No git and no LSP, though both are the point — see
  [RECOMMENDATION.md](RECOMMENDATION.md). LSP hangs off `Editor::sync_syntax`,
  the same edit drain tree-sitter uses.

### Architectural, and cheaper to fix now than later

- **Tree-sitter is not optional, so building needs a C toolchain — and twenty
  grammars is a 23.6 MB binary, up from 4.75 MB with Rust alone.** Grammars are
  C compiled by `cc`, which breaks minimal containers and makes
  cross-compilation harder, and each one is a large generated parser table. A
  Cargo feature per grammar fixes both: `Editor::syntax` is already
  `Option<Syntax>` and an unknown file name already renders as plain text, so
  the no-syntax path exists and works.
- ~~**The config language is undecided**~~ Decided and mostly built: TOML, in
  `~/.config/bi/config.toml`, parsed by `bi::config`. `[options]` is live, and
  so are `[keys.*]`, sequences and `<leader>`. What is left is that a binding
  resolves to bi's own keys rather than to a command, so the defaults still
  live in `input.rs`; the highlight table in `tui/render.rs` is still hardcoded
  and is step 2 of [docs/specs/config.md](docs/specs/config.md).
- **The core/frontend boundary is enforced by a test, not the compiler.** Fixed
  as far as it goes: there is a `lib.rs`, `input.rs` speaks `bi::key::Key`
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
