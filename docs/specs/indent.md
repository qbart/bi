# Indentation

bi did not know what an indent was. `TAB_WIDTH: usize = 4` was a `const` in
`src/tui/render.rs`, `Tab` in insert mode inserted a literal `\t`, there was no
`>` and no `<`, and `o` on an indented line opened one at column zero. That is
not a missing feature so much as a missing noun: `.editorconfig` has nothing to
configure, indent guides have nothing to draw at, and `:set shiftwidth` has
nothing to set until indentation is a thing the core has a word for.

This is that word. Everything downstream — `.editorconfig`, the vertical
guides, trimming — is written against what is here.

## Status

**Built.**

## Where it lives

In the library, not the frontend. The width of a tab decides three things at
once: where the cursor is drawn, where an indent guide goes, and how far `>`
moves a line. Two of those are editor semantics and only the first looks like
rendering, so a constant in the renderer was always the wrong home for it — a
second frontend would have had to guess the same number and would eventually
have guessed differently.

`src/indent.rs` holds the arithmetic and nothing else: it takes a `&str` and a
width and answers questions about columns. `Buffer` holds the edits, `Options`
holds the settings, and the renderer asks rather than assumes.

## The options

```
tab_width  = 4      how wide a \t is drawn
expandtab  = true   an indent is written as spaces
shiftwidth = 0      how far > moves; 0 means "whatever tab_width says"
autoindent = true   a new line starts under the one above it
```

**`shiftwidth = 0` means `tab_width`.** Vim's rule, and worth keeping: almost
nobody wants their `>` step to differ from their tab stop, and a default of 0
means one knob controls both until someone deliberately wants two. `:set
shiftwidth` reports the 0 rather than the 4 it resolves to, because reporting 4
would be reporting a value that is not set and that `:set tab_width 8` is about
to change.

**`expandtab` defaults to true**, which is not vim's default. Vim's default is
from 1991 and every language that has arrived since has picked spaces; a
default of tabs means bi writes a character most repositories have configured
their tooling to reject.

The exception it costs is real and is named here so it is not a surprise: a
`Makefile` requires tabs, and until per-file options land (`docs/specs/`
`editorconfig.md`) a Makefile in a repository with no `.editorconfig` needs
`:set expandtab false` first. That is one commit's worth of hazard, taken
deliberately rather than defaulting to tabs everywhere to protect one filetype.

## What an indent is made of

```rust
pub struct Indent {
    pub tab_width: usize,
    pub expandtab: bool,
    pub shiftwidth: usize,
    pub autoindent: bool,
}

impl Indent {
    /// How far one `>` moves. `shiftwidth`, or `tab_width` when it is 0.
    pub fn step(&self) -> usize;
    /// `width` columns of indentation, written the way the options ask for.
    pub fn render(&self, width: usize) -> String;
}
```

`render` is the whole of the tabs-versus-spaces policy, in one place, so that
`>`, `Tab`, and autoindent cannot disagree about it:

- `expandtab` — `width` spaces.
- otherwise — `width / tab_width` tabs, then `width % tab_width` spaces.

The remainder is spaces because there is no such thing as most of a tab. It
only ever appears when `shiftwidth` does not divide `tab_width`, which is a
combination someone has to ask for explicitly, and when they do the file still
lines up on screen — which is the only promise indentation makes.

Widths are always in **columns**, never in characters. A line indented with one
tab and one space is 5 columns wide at `tab_width = 4`, and `<` takes it to 0
rather than to "one character less", because what the eye reads is the column.

## The operators

```
>{motion}   >j  >ip  >>      indent
<{motion}   <k  <i{  <<      outdent
```

Both are operators in the full sense — they take a motion or a text object,
they double (`>>`), they take a count, and `.` repeats them. That is not
generosity, it is the only way they can work: `>` is spelled as an
[`Operator`](../../src/motion.rs) precisely so the machinery that already knows
`d` waits for a motion knows `>` does too, rather than growing a second pending
state beside the first.

**Always linewise**, whatever the motion says. `>w` indents the line the word
is on; there is no such thing as indenting half a line, so the char range a
motion produces is widened to the rows it touches.

**The count is lines, in normal mode.** `3>>` indents three lines one step, and
`>3j` indents four — the same folding every other operator does, because the
count belongs to the motion.

**The count is steps, in visual mode.** `3>` moves the selection three steps
right. That is vim, and it is the more useful of the two readings: the rows are
already named by the selection, so a count that meant rows would have nothing
to say.

**Visual `>` keeps the selection.** Vim drops it, and every vimrc in the world
puts it back with `vnoremap > >gv`. Keeping it is also what makes `3>` fall out
for free rather than needing a rule: three steps *is* the command run three
times, and it can only run three times if the selection survives the first.

**An empty line is not indented.** Vim's rule, and the reason is trailing
whitespace: a line with nothing on it gains nothing from being pushed right,
and a file full of `    \n` is a file full of diff noise. A line that is only
whitespace *is* emptied by `<`, though — outdenting to column zero is the one
way to get rid of it.

**The cursor lands on the first non-blank** of the first line touched, which is
vim, and which means a repeated `>>` keeps pushing the line it is looking at
rather than sliding off the end of the indent it just made.

**`=` is deliberately absent.** Reindenting to what the language thinks is
correct needs tree-sitter indent queries, which is its own spec and its own
pile of per-grammar behaviour. `>` and `<` say what *you* want; `=` says what
the grammar wants, and those are different features that happen to share a
neighbourhood on the keyboard.

## Insert mode

```
Tab           forward to the next indent stop
Shift-Tab     back one indent stop
Backspace     back one indent stop, when there is only whitespace behind it
```

**`Tab` aligns rather than inserts.** It moves to the next multiple of
`step()`, which at column 6 with a step of 4 means two columns, not four. This
is what a tab has always meant on a terminal and it is what makes columns line
up; inserting a fixed width instead would make `Tab` twice in a row produce a
different result from `Tab` at the same place on the line below.

**`Shift-Tab` is the inverse and only that.** It removes back to the previous
stop when the text behind the cursor is whitespace, and does nothing at all
otherwise — it never deletes a character you typed. Terminals send it as
`BackTab`; the frontend translates that to `Tab` with `shift`, which is what it
is, rather than the core growing a key code for one binding.

**`Backspace` eats a whole indent** when everything to the left of the cursor
on that line is whitespace. With `expandtab` this is the difference between one
press and four, and without it the behaviour is unchanged because one tab
already is one step. Anywhere else on the line, `Backspace` is exactly what it
was: one character.

## Auto-indent

`o`, `O` and `Enter` copy the leading whitespace of the line they came from,
verbatim — the characters, not the width. Copying the characters is what keeps
a tab-indented file tab-indented even when `expandtab` is on: the new line
matches its neighbour, which is what "auto indent" is for. A file only changes
its indent character when you ask for one with `>` or `Tab`.

`Enter` splits a line, so it copies the indent of the line being split, and it
copies it whole even when the cursor is inside the indent — the text that moves
down keeps its own leading space and gains the copy, which is what makes
splitting a line in the middle of its indent harmless.

**An abandoned indent is removed.** Leaving insert mode on a line that is
nothing but whitespace clears it. Without this, every line you opened and
thought better of leaves an invisible `    ` behind, and the diff shows it even
though the screen never did. Gated on `autoindent`: with it off, nothing put
that whitespace there but you, and the editor should not second-guess it.

## Guides

```
indent_guides = true
```

A vertical line down each level of indentation, at columns 0, one step in, two
steps in, and so on up to — but not including — the text. Column 0 is a level
like any other; the text's own column is not, because a guide there would sit
on the first character of the line rather than in the whitespace before it.

They are drawn as *overlay decorations* (`docs/specs/decorations.md`), which is
what lets one land at column 4 of a tab-indented line — a position that is
inside a tab and has no char offset to be anchored to. The line comes out the
same length it went in, so nothing after a guide moves.

**A blank line shows the guides of the smaller of its nearest non-blank
neighbours.** A blank line inside a block keeps the block's guides; one between
a block and what follows it shows none, because it belongs to neither. `min`
rather than `max` is the whole of that rule, and the scan stops at the first
non-blank line in each direction.

The character is `│`, and it is not an option yet. Vim spells the same idea
`listchars`, which is a grammar for a handful of characters and is not worth
copying for one; if a font somewhere cannot draw it, that is the day for
`indent_guide = "|"`.

## Display columns

```rust
pub fn display_col(line: &str, char_col: usize, tab_width: usize) -> usize;
pub fn expand_tabs(line: &str, tab_width: usize) -> String;
pub fn width_of(text: &str, tab_width: usize) -> usize;
pub fn leading(line: &str) -> &str;
```

Moved out of `src/tui/render.rs` unchanged in behaviour, changed in ownership.
The renderer keeps calling them; it now passes the width it was told instead of
the one it knew.

The inherited caveat comes with them: width is counted in `char`s, so wide
(CJK) and combining characters are still off by however much they differ. That
is a `unicode-width` dependency and a grapheme walk, it is the same bug it was
before this spec, and it is worth fixing before bi is usable on non-Latin text.

## What this leaves for later

**Per-file options.** `expandtab` is one setting for the session, so a Makefile
and a Python file in the same session cannot disagree. `.editorconfig` is the
next spec and it is where the resolution chain — built-in filetype defaults,
then config, then `.editorconfig`, then an explicit `:set` — is built.

**Indent guides** landed with the decoration layer — see the section above.

## Tests

- `step()` follows `tab_width` at 0 and `shiftwidth` otherwise.
- `render` writes spaces under `expandtab`, and tabs plus a spare space when
  the step does not divide the tab width.
- `>>` and `<<` on one line, with both settings, from both a tab-indented and a
  space-indented file.
- `<` subtracts a step rather than rounding to one: 6 columns with a step of 4
  goes to 2, not to 4. Vim does the same, and rounding would make `>` and `<`
  fail to undo each other on a file that was never aligned to the step.
- `<` clamps at column zero rather than eating text.
- `3>>` covers three lines; `>3j` covers four; `>ip` covers the paragraph.
- Visual `3>` moves three steps and the selection survives.
- An empty line inside an indented range is left alone by `>` and emptied by
  `<`.
- The cursor lands on the first non-blank.
- `.` repeats `>>`.
- `Tab` at column 6 with step 4 inserts two columns.
- `Shift-Tab` with text behind the cursor does nothing.
- `Backspace` in leading whitespace takes a step; on the line's text it takes a
  character.
- `o` and `Enter` copy a tab indent as a tab even with `expandtab` on.
- Leaving insert mode on a whitespace-only line clears it, and does not with
  `autoindent` off.
- Guides at every level and never on the text; a ragged indent still gets the
  level below it.
- A blank line takes the smaller of its neighbours, and none at all where a
  block ends.
- A guide lands inside a tab's expansion, and the line it lands on is the same
  length afterwards.
- The renderer holds no width of its own: the same text laid out at two widths
  comes out two lengths, which is only possible because the number arrives from
  the options rather than from a constant beside the drawing code.
