# `:case`

`helloWorld`, `hello_world`, `HelloWorld`, `HELLO_WORLD` and `hello-world` are
one name in five spellings. Moving between them by hand is four commands and
one character wrong.

```
:case snake
:case camel     :case pascal    :case dash     :case const
:case upper     :case lower     :case title
```

**One name each, and no aliases.** An earlier draft took `capital` for `title`,
`screaming` for `constant` and `dash` for `kebab`. A second name for a style is
a second thing in the error message and a second thing to keep true, and it
buys nothing that completing the one name does not — so `kebab` is `dash`,
which says what the separator is, and `constant` is `const`, which is what the
languages that want it call it.

## Status

**Built.**

## What it acts on

**Whatever the scope on the `:` line names**, and the line says which — see
[ranges.md](ranges.md). Pressing `:` with a selection up prefills `'v`, so the
common case is "the selection" and it is visible before you press Enter:

| | |
|---|---|
| `:'v case snake` | exactly what is selected — a rectangle's columns, a charwise selection's characters, every cursor's own |
| `:'<,'>case snake` | the *rows* the selection touches, whole |
| `:2,5case snake` | lines 2 to 5, whole |
| `:case snake` | the word under each cursor |

**The word under the cursor is the no-scope default.** Renaming one is what
this is for, and selecting it first would be a keystroke that says nothing new.
A cursor on whitespace has no word: `iw` there is the run of blanks, which is
right for `diw` and is not a name, so `:case` says so rather than quietly doing
nothing.

There is no shape arm here at all. `:case` walks a region a row at a time —
never across a line terminator — and a rectangle is simply a region whose rows
are five columns wide. See [regions.md](regions.md).

The cursors are **carried across** the edit rather than replaced: they were
where you put them, and respelling the text under one is not a reason to move
it somewhere else. That is `Edit::map`, the same carry `:retab` and the
trimmer use.

## What it does to the text

Every **identifier** in range is respelled, and everything between them is left
exactly as it was. `foo_bar baz_qux` in camel is `fooBar bazQux`, not
`fooBarBazQux`: the second reading is what "convert this to camel" literally
says and is never what anybody means.

That is also why `:case` over a whole line respells the keywords on it — they
are identifiers too. Select the name.

`upper`, `lower` and `title` are pure case mappings and touch nothing else, so
`:case upper` on `Hello, World! 42` is `HELLO, WORLD! 42`, punctuation, spacing
and digits intact.

## Reading a name apart

Three boundaries, and between them they read every spelling back the same way:

- a separator — `_` or `-`
- a lower-to-upper step — `helloWorld`
- the last capital of a run before a lowercase one — `HTTPServer` is `http` and
  `server`, not `httpserver` and not `h t t p server`

Digits stay with the word they follow, so `utf8` is one word and
`parseUTF8Text` is three.

A `-` counts as a separator only *between* two identifier characters, so
`hello-world` is one name while `a - b` is two names and a minus sign. Without
that rule, `:case snake` over an expression would eat the arithmetic.

## Why an ex command

`:case` rather than a key, because there are eight of them and a key per style
is eight keys spent on something you do a few times a day. The command line is
where a thing with a name and an argument belongs, and `:case ` plus a word is
already shorter than remembering which of `gU`, `gu`, `g~`, `crs`, `crc` and
`crm` is which.

## Tests

- Every spelling reads back to every other one.
- An acronym is one word; a digit stays with its word.
- Each identifier converts on its own, and the text between them is untouched.
- `a - b` keeps its minus sign.
- The word under the cursor when nothing is selected; every selection when
  something is; the cursor on the first character afterwards.
- Whitespace under the cursor is not a word.
- One undo step.
- An unknown style lists the ones that exist and changes nothing, and the
  dropped aliases are unknown styles.
