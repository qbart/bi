# The `:` line

The `:` line was a `String` you could append to and backspace from. Everything
else a prompt has been able to do since readline was written — move left, go to
the start, walk what you ran before — was missing, and the way you found out was
typing `:%s/2024/2025/g`, seeing one wrong character, and holding Backspace.

This gives it a cursor and an Up arrow.

## Status

**Built.**

## The keys

| Key | Does |
|---|---|
| `Left` `Right` | move the cursor one character |
| `Home` `End`, `Ctrl-A` `Ctrl-E` | to the start of the line, to the end |
| `Up` `Down` | older / newer, out of the command history |
| `Backspace` | delete the character before the cursor; on an empty line, leave |
| `Ctrl-R` | the history picker — see [cmdline-history.md](cmdline-history.md) |
| `Enter` | run it |
| `Esc` `Ctrl-C` | leave |

**Arrows, not `hjkl`.** There is no normal mode on a `:` line to put motions
in: every printable key you press is a character of the command, which is what
makes a prompt a prompt. `Ctrl-A` and `Ctrl-E` come from the shells rather than
from vim — the fingers that reach for them are already at a prompt, and vim's
own answer (`Ctrl-B`, `Ctrl-E`) is a pair nobody outside vim knows.

**`Ctrl-A` is not vim's.** In vim `Ctrl-A` on a command line inserts every
matching filename, which is a feature nothing here has. Taking the key costs
nothing and buys the one every other prompt on the machine has.

## The type

```rust
pub struct CmdLine {
    text: String,
    /// Characters, not bytes: the cursor is a screen position.
    at: usize,
    recall: Option<(usize, String)>,
}
```

`Mode::Command(String)` becomes `Mode::Command(CmdLine)`. It derefs to `str`
and displays as one, so the parser, the history and the renderer read it
exactly as they read the `String` — the whole change is that it now knows where
the cursor is.

**`at` counts characters.** A byte index would put the cursor inside a `ü`, and
every consumer of this wants a column anyway.

**`recall` is not compared.** `PartialEq` is written by hand over `text` and
`at` only. Where a history walk has got to is not a fact about what the command
line *is* — two lines showing the same text with the cursor in the same place
are the same line — and comparing it would make every existing
`Mode::Command("w".into())` assertion depend on state the test never set.

## The history walk

`Up` steps to an older line, `Down` to a newer one, over
`Session::cmd_history` — the same list `Ctrl-R` opens as a picker, newest
first.

**The first `Up` saves what you typed.** `Down` past the newest entry puts it
back, exactly as it was. A prompt that eats a half-written command in exchange
for showing you an old one is a prompt you stop pressing `Up` at.

**The ends do not wrap.** `Up` at the oldest entry stays there and `Down` with
no walk in progress does nothing. Wrapping a list you cannot see turns "I have
gone too far" into "I am somewhere else now".

**Editing ends the walk.** Once you have typed into a recalled line it is
yours, and `Down` no longer offers to take it away. The draft is dropped with
it — the line you are looking at is the line you meant.

**No prefix filter.** `Up` after `:w` offers the whole history rather than only
the lines starting `w`. That is the feature `Ctrl-R` already is, done better:
it filters on every term, shows you the list, and does not make you guess how
many times to press a key. `Up` is for the last thing or the one before it.

## The selection comes back

Pressing `:` with a selection up prefills `'v ` and remembers the shape it
interrupted (`Session::interrupted_visual`). What happens to that selection
when the line is done depends on what the line did:

- **A command that consumed it** — `:'v case snake`, `:'v s/a/b/` — collapses
  the selections as part of its edit and the editor stays in normal mode.
  That was already true.
- **Anything else returns to visual mode.** A command that failed
  (`:'v case invalid`, an unknown name, `no line 99`), a command that had no
  use for the selection (`:w`), a command that deliberately kept it
  (`:'v m +1` keeps the moved block selected so you can move it again), `Esc`
  on the line, and backspacing off the front of it — in every one of these
  the selection is still standing, so the mode says so.

**The rule is one test, not a list**: after the `:` line closes, if the
window still holds a selection with room in it, the editor is back in the
visual mode it interrupted. There is no registry of which commands fail and
which succeed — a command that consumes the selection collapses it, and the
collapse is the signal. That matters because the renderer already paints every
uncollapsed selection whatever the mode: without this rule a failed
`:'v case invalid` left the selection *painted* but the mode normal, so the
next `:` prefilled nothing and the retyped command quietly acted on the word
under the cursor instead of the selection you were looking at.

## Where it does not apply

The `/` and `?` lines keep their append-and-backspace `String`. They want the
same treatment and their own history, and `cmdline-history.md` has already
written down what that costs; doing it here would be doing it twice with the
question of whether the two lists are one still open.

## Tests

In `editor.rs`:

- typing lands at the cursor, not at the end: `Left` twice then a character
  puts it two from the right.
- `Backspace` takes the character *before* the cursor, and does nothing at
  column 0 on a line with text on it; on an empty line it still leaves command
  mode.
- `Ctrl-A` and `Ctrl-E` reach the ends, and `Left` at 0 and `Right` at the end
  stay put.
- `Up` puts the last command on the line with the cursor at its end; `Up`
  twice reaches the one before; `Down` walks back; `Down` past the newest
  restores the half-typed draft.
- `Up` with an empty history leaves the line alone.
- typing after a recall ends the walk: `Down` then does nothing.
- executing a recalled line runs it and records it at the front.
- a failed `:'v case invalid` returns to visual mode with the selection
  standing, and the next `:` prefills `'v ` again; so do an unknown command,
  `Esc` on the line, and backspacing off the front of it.
- `:'v case snake` still ends in normal mode — consuming the selection
  collapses it, and the collapse is the signal.
- `:'v m +1` returns to visual mode with the moved block selected.
- the shape survives: a rectangle interrupted is a rectangle restored.

In `input.rs`:

- `Left`, `Right`, `Home`, `End`, `Up`, `Down`, `Ctrl-A` and `Ctrl-E` on the
  `:` line each produce their action rather than being swallowed as text.

In `render.rs`:

- the terminal cursor sits at the `:` line's own cursor, not at the end of it.
