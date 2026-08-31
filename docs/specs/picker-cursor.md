# A cursor on the picker's query

The picker's query was a `String` you could append to and backspace from —
the same prompt-before-readline shape the `:` line started as, with the same
failure: one wrong character in the middle of `animcurve` and the only way
back is holding Backspace through everything after it.

This gives the query the cursor the `:` line already has, by giving it the
same type. `CmdLine` is a line of text with a character cursor; that it grew
up on the `:` prompt does not make it about commands. The picker holds one,
delegates movement to it, and keeps matching against what it says.

## Status

**Built.**

## The keys

| Key | Does |
|---|---|
| `Left` `Right` | move the cursor one character |
| `Home` `End` | to the start of the query, to the end |
| typing | inserts at the cursor |
| `Backspace` | delete the character before the cursor; on an empty query, cancel |

Every picker — registers, files, buffers, history, all of them — gets these at
once, because they are one widget.

**`Up` and `Down` stay on the list.** On the `:` line the vertical arrows walk
the history because there is nothing else vertical there; in a picker the list
is the whole point, and the arrows keep moving the highlight as they always
have. Horizontal for the query, vertical for the list.

**No `Ctrl-A`/`Ctrl-E`.** The `:` line takes the shells' pair; the picker
cannot — `Ctrl-A` is already the reveal-short-entries toggle, and a `Ctrl-E`
whose partner does something unrelated is a trap. `Home` and `End` are the
spellings here.

**Backspace at column 0 of a non-empty query does nothing.** It used to be
impossible to be anywhere but the end, so "nothing left to delete" and "the
query is empty" were the same fact and either one meant cancel. With a cursor
they come apart, and the `:` line already chose: leaving is what an *empty*
line's Backspace means, column 0 of a line with text on it is just nothing to
delete. The picker matches.

## The type

`Picker.query` becomes a `CmdLine`. `query()` still returns `&str` — the
matcher, the renderer and every test read the text exactly as before — and
`cursor()` says which column the frontend should put the terminal cursor in,
counted in characters so a `ü` cannot split it. The recall half of `CmdLine`
goes unused here; the picker's `Up` is taken, and the list *is* its history.

`Action::PickMove(CmdMove)` carries the four movements, the same payload the
`:` line's `CommandMove` carries.
