# Surround

Changing `"hello"` to `'hello'` is three commands and a moved cursor in stock
vim, and one keystroke sequence — `cs"'` — in the plugin everybody installs.
The plugin is right, and there is nothing in it that needs to be a plugin.

## Status

**Built**, without tags. See *What is not here*.

## The three commands

```
ys{motion}{char}    wrap what the motion covers
yss{char}           wrap the whole line
ds{char}            delete the surrounding pair
cs{old}{new}        change one pair into another
S{char}             in visual mode: wrap the selection
```

They are spelled the way vim-surround spells them, and the spelling is not an
accident: `s` is not a motion, so `ys`, `ds` and `cs` are all sequences vim
leaves unused. Nothing has to be given up to have them.

`ys` reads as "you surround"; it borrows the yank operator's key because yank
is the operator that changes nothing, and neither does the `y` in `ys`.

**Every one of them keeps the cursor where it is**, moved only by whatever was
inserted or removed to its left. `cs"'` from inside a string leaves you on the
same character you were on, which is the whole reason to have `cs` rather than
doing it by hand.

## The characters

| You type | You get |
|---|---|
| `(` `{` `[` `<` | `( x )` — the open side adds a space inside |
| `)` `}` `]` `>` `b` `B` | `(x)` — the close side does not |
| `"` `'` `` ` `` | the same character on both sides |

The open-adds-a-space rule is vim-surround's, and it earns its keep: `ys iw {`
around a Rust or JavaScript identifier gives `{ x }`, which is what the
formatter would have written, while `}` gives `{x}` for the times it would not.

Any of the six bracket characters *finds* the same pair — `ds(`, `ds)` and
`dsb` all delete the nearest enclosing parentheses. Only writing distinguishes
them.

## What it does to the text

**`ys`** inserts the closing string, then the opening one, so the second
insertion cannot invalidate the first's position. The range comes from the same
motion and text-object machinery every operator uses, which is why `ysiw"`,
`ys2w)`, `ysa{` and `ysip>` all work without a line of their own.

**`ds`** finds the innermost pair the cursor is inside — the same search `di(`
and `di"` already do, so nesting is already handled — and removes exactly the
two delimiter characters. Not the whitespace inside them: `ds(` on `( x )`
leaves ` x `, because removing a space someone typed on purpose is worse than
leaving one they did not.

**`cs`** replaces the two characters in place. `cs"'` costs two edits and moves
nothing else, which is the property that makes it worth a command.

All three are one undo step, and `.` repeats them.

## What is not here

**Tags.** `dst`, `cst` and `ys{motion}t` are vim-surround's fourth kind, and a
tag is not a pair of characters — it is a name that has to be read out of the
opening tag and matched, with nesting, against a closing one. bi has grammars
for HTML and XML and no way to reach them from a file the grammar does not
cover, which makes "find the enclosing tag" a parse rather than a scan. It
belongs with the tree-sitter selection work (`S`, in TODO.md), where the same
question is already being asked, rather than as a second scanner here.

**Custom pairs.** vim-surround lets a filetype define its own; nothing here
does yet, and the option would be a table of two-string pairs keyed by
character. Nobody has asked for one.

**`ySS` and the linewise forms** that put the surroundings on their own lines.
They exist in the plugin; they are rare, and each is a separate rule about
indentation that is better added when someone wants it than guessed at now.

## Tests

- `ysiw"` around a word, `ys$)`, `ysip{`, and `yss"` on a whole line.
- The open side adds a space inside and the close side does not.
- `ds"`, `ds(` and `dsb` all find the nearest enclosing pair, from inside it
  and from on top of a delimiter.
- `ds` on a line with no such pair changes nothing.
- `cs"'` from inside the string leaves the cursor on the character it was on.
- `cs)(` adds the spaces the open side promises.
- Nesting: `ds(` inside `f(g(x))` takes the inner pair.
- Visual `S` wraps the selection and returns to normal mode.
- One undo step each, and `.` repeats a `ys`.
