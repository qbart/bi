# `S` — select by structure

`vi(`, `va{`, `vi"` and `vip` each ask you to know two things before you press
anything: which bracket, and whether you want the inside or the whole of it.
The parse tree already knows what is around the cursor, in order, from the
tightest thing out to the file itself.

`S` shows that list as letters and lets you pick one.

## Status

**Built.**

## What you see

Cursor inside the string, in Lua:

```lua
{ "hello/plugin" },
```

`S` marks both ends of every scope around the cursor:

```
c{ b"ahello/plugina"b }c
```

- `a` — the contents of the string, the tightest thing there is
- `b` — the string, quotes included
- `c` — the table, braces included

Press `b` and that becomes the selection, in charwise visual mode.

**Both ends carry the same letter**, which is what makes the list readable:
one letter tells you where a scope starts *and* where it ends, so you can see
how much you are about to select before you select it.

**`a`, `b`, `c` — the alphabet, not the home row.** Everywhere else in bi a
label is chosen for the finger; here it is chosen for the *order*, because the
scopes are a nesting and `a` inside `b` inside `c` says so at a glance. This is
the one client that passes its own key order to `label::labels`.

## What counts as a scope

The chain of tree-sitter nodes containing the cursor, innermost first, with
consecutive duplicates dropped — a node whose only child covers exactly what it
covers is one boundary wearing two hats, and offering it twice would waste a
letter and confuse the nesting.

That is the whole rule, and it is why the example above needs no special case
for strings, brackets, or anything else: `string_content` inside `string`
inside `table_constructor` is what the Lua grammar says, and the letters follow
it. The file itself is the last scope, so the last letter always selects
everything.

**No grammar, no scopes.** A file bi has no parser for has no structure to
offer, and `S` says so rather than guessing at brackets. That is the one thing
this cannot fall back on: `vi(` still works there and always will.

## What `S` cost

Vim's `S` is `cc` spelled shorter, and `cc` still spells it. In visual mode `S`
stays vim-surround's, which is a different key in a different mode.

## How it is drawn

Two `Overlay` decorations per scope, on the first and last character of its
range, on the `Over` layer, in the theme's `label`. Outermost first, so where
two scopes share an edge the tighter one's letter is the one you see — it is
the one you are more likely to want, and the outer one still shows at its other
end.

## Tests

- The Lua example, exactly: three scopes, `a` innermost, both ends marked.
- A node whose range equals its parent's is offered once.
- Pressing a letter selects that range in charwise visual.
- The letters are `a`, `b`, `c` in nesting order.
- A file with no grammar offers nothing and says so.
- `Esc` and a key that is no letter both cancel and leave the cursor alone.
