# `:case`

`helloWorld`, `hello_world`, `HelloWorld`, `HELLO_WORLD` and `hello-world` are
one name in five spellings. Moving between them by hand is four commands and
one character wrong.

```
:case snake
:case camel     :case pascal    :case kebab    :case constant
:case upper     :case lower     :case title
```

`capital` is `title`, `screaming` is `constant`, and `dash` is `kebab` — the
same styles under the names people reach for.

## Status

**Built.**

## What it acts on

**The selection**, if there is one — including every selection, so a column of
cursors respells a column of names.

**The word under the cursor**, if there is not. Renaming one is what this is
for, and selecting it first would be a keystroke that says nothing new. A
cursor on whitespace has no word: `iw` there is the run of blanks, which is
right for `diw` and is not a name, so `:case` says so rather than quietly
doing nothing.

Whichever it is, the cursor lands on the first character of what was respelled.
The text under where it was is a different length now, and the start is the one
position that means the same thing before and after.

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
- An unknown style lists the ones that exist and changes nothing.
