# Tree-sitter context, as virtual text

A closing brace says nothing. Twenty lines into a function, inside a loop,
inside a match arm, the `}` under the cursor is one of four that look the same,
and finding out which one it is means scrolling up until the indentation stops
looking familiar. The parse tree already knows: `S` asks it what is around the
cursor and gets back the chain of nodes, innermost first
(`docs/specs/scopes.md`).

This puts the answer on the screen. The line that opened the block you are in
is repeated, as a comment, after the line that closes it.

## Status

**Built.**

## What you see

Cursor inside the `if`:

```c
if (value == 0) {
    CURSOR
} // if (value == 0) {
```

The text after the `}` is not in the file. It is the *opening line*, copied
verbatim, behind that language's line-comment marker.

Lua and Python, which close their blocks differently or not at all:

```lua
function greet(name)
    CURSOR
end -- function greet(name)
```

```python
if value == 0:
    CURSOR
    do_something()  # if value == 0:
```

Python is the honest case and the one worth looking at: a Python block has no
closing line, so it ends on its last statement and the annotation goes there.
That reads as a comment on a line of code rather than on a delimiter, which is
slightly odd and is *true* — that line is where the block ends. Inventing a
row to hang it on would be worse.

## Which block

**The innermost one containing the cursor**, and `context_depth` says how many
to walk outwards from there. At the default of 1 there is one annotation on
screen at a time, which is the setting this feature is for: the question being
answered is "what am I in", singular. At 3, a nested loop inside a function
inside an `impl` marks all three closers, and the screen starts telling you
about structure you can already see.

`context_depth = 0` turns it off. That is the same spelling as `yank_flash =
0`, and for the same reason: a feature whose size is a number does not need a
second option saying whether the number counts.

**Nothing is shown while the cursor is on the opening line.** The line the
annotation repeats is then directly under the cursor, and repeating it three
rows down is noise about something already in view. So a block qualifies only
when its opening row is strictly above the cursor's row — which still includes
sitting on the closing brace itself, where the annotation is exactly what you
want.

## The rule

1. `Syntax::scopes_at(cursor_byte)` — the byte ranges of every node containing
   the cursor, innermost first.
2. Each becomes a row pair, `(start_row, end_row)`. `end_row` is the row of the
   node's *last byte*, not of its exclusive end: a node finishing at a line
   break ends on the row before the one that break starts.
3. A pair survives if `end_row - start_row >= context_min_lines` and
   `start_row < cursor_row`.
4. Pairs that repeat are dropped, keeping the first. In C, `if (v) { … }` is an
   `if_statement` whose `compound_statement` child starts on the same row and
   ends on the same row: one block wearing two hats, and annotating it twice
   would print the same line twice on the same closer.
5. `start_row` must be less indented than the first non-blank row inside the
   block — see below.
6. The text is the *line* at `start_row`, trimmed at both ends, walking up past
   any row that is nothing but punctuation.
7. The first `context_depth` survivors are the answer.

**The line, not the node.** A node's own text starts where the node starts,
which for a C `compound_statement` is the `{` — annotating it with `{` would
be a joke. Taking the whole row instead means the condition, the function
signature, the `match` scrutinee and the decorator-free `def` line all arrive
without one arm per grammar. It also means a block whose opener spans two
physical lines shows only the first of them, which is a real limitation and the
right trade: one row of virtual text, always, never a wrapped paragraph pushed
into the buffer's margin.

**Row pairs, not byte ranges, are what dedup keys on.** Two nodes with
different byte ranges that begin and end on the same rows are indistinguishable
here — the annotation is a row and a line of text, and it would be the same
annotation twice.

### Step 5, which is the part that is a judgement

Not every node's first row *opened* anything. Python's `block` starts at the
first statement of the `if`, not at the `if`:

```python
if name:          # row 1 — the if_statement starts here
    print(name)   # row 2 — the block starts here
    print(name)   # row 3 — and both end here
```

Both nodes close on row 3, so nothing about rows tells them apart, and
annotating the `block` writes `# print(name)` — a comment naming the line above
it. The two are distinguishable only by node *kind*, which means a query file
per grammar: thirty files, upstream-maintained, to answer one question.

**Indentation answers it in one comparison.** A row that opens a block is less
indented than the rows inside it. Row 2 is not less indented than row 3, so it
opened nothing; row 1 is, so it did. That is the same assumption the indent
guides already make about a file, applied to the same file.

The price is honest: a block written with no indentation inside it gets no
annotation. That file has a bigger problem than a missing comment.

Indentation is counted in leading whitespace *characters*, a tab as one. The
only question ever asked is which of two rows in one block is further in, and a
file that indents one of them with tabs and the other with spaces is already
lying to every other tool that reads it.

### Punctuation is not a name

```c
int main(void)
{
    do_something();
}
```

The block opens on the brace, and `} // {` is a joke. So the opening row walks
*up* to the nearest row that contains a letter or a digit, which is the
signature — the line a reader would have said out loud. In the common brace
style the walk stops immediately, because `if (value == 0) {` names something
on its own.

## How it is drawn

One `Decoration::Eol` per surviving block, on `end_row`, in the theme's
`context`. Past the end of the line, which is the one place virtual text costs
nothing: `Eol` adds no cells to the row's own text, so the search highlight,
the selection, the block-visual arithmetic and the indent guides all go on
seeing the row as the file says it is. `Inline` would have been wrong twice
over — see `docs/specs/decorations.md` on why nothing that survives a keystroke
may use it.

Produced in `Editor::decorations`, filtered to the rows on screen, and **only
for the focused window**. An unfocused pane's cursor is not where you are
looking; annotating from it would put a comment on a brace for reasons off the
screen.

Nothing is cached, like every other decoration: it is derived from the parse
tree and the cursor, both of which are already to hand, and the walk is one
`descendant_for_byte_range` and a parent chain.

**No grammar, no context.** A file bi has no parser for gets nothing, the same
answer `S` gives, and for the same reason: guessing at braces is how an editor
tells you confidently about a block that is inside a string.

## The comment marker

`syntax::line_comment(filetype) -> Option<&'static str>` — `//` for the C
family, `#` for Python, shell, TOML and YAML, `--` for Lua and SQL, `;` for
Lisp and ini-shaped files, `%` for TeX. One arm per language beside
`syntax::filetype`, which is where the language table already lives.

**Per language rather than a fixed `//`.** The annotation is meant to read as a
comment, and `// end` in a Python file reads as a mistake in the file. A
configurable prefix would have been one setting to get wrong per project;
the filetype is already known, and it is the same fact.

A filetype with no line comment — a grammar exists but nothing sensible marks a
comment in it — gets no annotation rather than a borrowed `//`. `None` is
returnable, so this is a case rather than a guess.

This function has an obvious second client: a comment-toggle key. It is written
where that client will find it.

## Options

Flat, like every other option (`docs/specs/options.md`):

| option | default | what it does |
|---|---|---|
| `context_depth` | `1` | How many enclosing blocks are annotated, innermost first. `0` is off. |
| `context_min_lines` | `1` | How many rows a block must span before it earns an annotation. `1` is any block that opens and closes on different rows. |

`context_min_lines` exists for the file that is nothing but three-line
guard clauses, where the annotations outnumber the code. It is not the default
because the three-line `if` is exactly the case the feature was asked for.

Both are per-buffer, through the ordinary options layering — a Python file and
a Rust file in two panes can disagree about the depth.

## Theme

One new `[ui]` key, `context`.

It is furniture in the sense `indent_guide` is: it marks structure rather than
naming anything in the text, and the moment it competes with the code for
attention it has failed. So each built-in sets it one step from its own
background, in whichever direction recedes there:

| theme | value | |
|---|---|---|
| `gruvbox-dark` | `#1d2021` | bg0_h — gruvbox's black, *below* the background |
| `gruvbox-light` | `#ebdbb2` | bg1 — just above the background |
| `pascal` | `#000080` | the darker blue the cursor line uses |
| `ansi` | `darkgray` | the tier `indent_guide` and `dim` already share |

`gruvbox-dark`'s is deliberately near-invisible: it was asked for as black, and
black against `#282828` is a shape you notice when you look for it and not
before. `:set` it or override `context` in a theme file to make it louder.

## What this is not

**Not a sticky header.** The other reading of "tree-sitter context" pins the
enclosing lines to the top of the viewport, over the buffer's own rows. That is
a different feature with a different cost — it eats screen — and it changes
which rows exist, which decorations deliberately cannot do. This one adds
nothing to the top of the pane and moves no cell.

**Not folding.** It repeats a line that is still there.

**Not written to the file.** It is a decoration. Yanking the closing line yanks
`}`.

## Tests

In `context.rs`, against a parsed buffer:

- The innermost block wins, and its text is the opening *line*, trimmed — not
  the node's text, which in C would start at `{`.
- A C `if` annotates once, not twice, though `if_statement` and
  `compound_statement` are two nodes over the same rows.
- `context_depth = 2` walks outwards and stops, innermost first.
- `context_depth = 0` produces nothing at all.
- `context_min_lines = 4` drops the three-row `if` and keeps the six-row
  function.
- The cursor on the opening line produces nothing; on the closing row it still
  produces the block, which is the case this exists for.
- A file with no grammar produces nothing.
- Python: the annotation names the `if`, not the first statement under it, and
  lands on the block's last statement row because that is where the block ends.
- Lua closes on its `end`.
- A brace on a line of its own is skipped for the signature above it.

In `editor.rs`:

- `decorations()` returns an `Eol` on the closing row, with the theme's
  `context` style and the language's marker, and returns nothing for that block
  when the closing row is scrolled out of view.
- An unfocused window produces none of them.

Through a rendered frame:

- The annotation appears after the closing brace, past the gutter, and the
  row's own text is unchanged — a decoration produced and not painted looks
  exactly like one never produced.
