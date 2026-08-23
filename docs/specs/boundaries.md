# Boundaries — `:ts`, `]]` and `[[`

The parse tree knows where every block begins and ends, and until now bi only
asked about the one under the cursor — `S` to select it, the context header to
name it. This asks about all of them: `:ts` paints every boundary on screen,
and `]]` / `[[` walk them the way `w` walks words.

## Status

**Built.**

## What counts as a boundary

Two kinds of node, and both of their ends:

- **Blocks** — any named node that spans more than one line. Functions,
  impls, structs, loops, `if`s, match arms, the file's top-level items: the
  multi-line span *is* the definition, so no per-grammar table of kinds is
  needed and every one of the thirty grammars is covered on day one. It is
  the same wager `contexts` and the indent guides already make about
  structure.
- **Argument lists** — a node whose kind contains `argument` or `parameter`,
  plus each named child it has. These are single-line and would be missed by
  the span rule, and they are the case the rule exists to serve: `]]` inside
  a signature hops argument to argument.

A boundary is a *position*: the first character of such a node, and its last.
Duplicates collapse — a function and the block that is its whole body share
an end, and one stop is one stop.

## `]]` and `[[`

`]]` moves to the next boundary after the cursor, `[[` to the previous one
before it. Starts and ends both count as stops, which is the `w`/`e` reading
asked for: the walk visits *each* boundary, so `]]` from a function's `fn`
lands on its `(`, then its first parameter, and eventually on its closing
`}` — and `[[` retraces the same stops backwards.

They live in the bracket family beside `]d` / `[d`, and follow its rules:
claimed only with no operator pending, plain navigation rather than operator
motions — `d]]` is not a thing until someone misses it. At the last boundary
`]]` stays put rather than wrapping: a boundary walk is local, and teleporting
to the top of the file is not walking. No syntax tree — a plain text file —
says `no syntax tree here`.

## `:ts` — see them all

`:ts` is a toggle. On, every visible boundary is painted: the screen dims —
the same `dim` the `s` jump uses, because both mean "the text is background
now, the marks are the subject" — and each boundary character wears the
`search` style, the same cell-lighting a match gets. `:ts` again turns it
off, and the status line says which way it went.

A toggle rather than `S`'s one-keystroke flash, by request: this is a mode
you read and move around in, checking `]]` against what it will do. The
decorations are recomputed every frame from the live tree, so edits made
while it is on stay honest — there is nothing to go stale. It dims only the
focused window, like `s`: the marks are about where *you* are looking.

In a buffer with no tree, `:ts` says `no syntax tree here` and stays off.

## Where it lives

- `Syntax::boundaries` — the walk. Byte positions out of the tree cursor,
  blocks by span and argument lists by kind, sorted and deduplicated. Beside
  `scopes_at` and `symbols`, which are the same kind of question.
- `Editor` turns positions into the two features: `Action::BoundaryJump`
  resolves the next/previous stop against the cursor, and `boundary_marks`
  produces the `:ts` decorations exactly the way `find_decorations` produces
  `s`'s — a dim `Repaint` under, a `search` `Repaint` per stop over it.
- The toggle is `session.ts_marks`, beside the other session-wide switches.

## Tests

- In a Rust buffer, `]]` from the top stops at the function, its parameter
  list, each parameter, its body — and `[[` walks the same stops back.
- `]]` at the last boundary stays put; `[[` at the first does too.
- A buffer with no tree says `no syntax tree here` for both the keys and the
  toggle.
- `:ts` toggles the flag both ways and reports each.
- With `:ts` on, decorations for the focused window contain the dim and a
  mark on a boundary; another window's decorations do not.
