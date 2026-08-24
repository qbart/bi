# Semantic split/join

`:tssplit` breaks the bracketed list around the cursor onto one line per
element; `:tsjoin` puts it back on one. The pair vim plugins spell as
splitjoin, done with the parse tree bi already holds, so "element" means what
the grammar says rather than what a regex guessed.

## Status

**Built.**

## The list at the cursor

Both commands work on the same node: the innermost tree-sitter ancestor of the
cursor whose first child is `(`, `[` or `{` and whose last child is the
matching closer. That single shape covers call arguments, parameter lists,
arrays, tuples, struct literals and import lists in every grammar shipped,
with no table of node kinds to maintain — the same wager `boundaries` makes.
A `{ … }` statement block qualifies too, which is deliberate: joining a small
block onto one line and splitting it back are exactly the moves the commands
name.

The cursor has to be inside the brackets (or on them): the ancestor walk finds
the list *around* the cursor, so `:tssplit` on the name of a call finds the
list enclosing the call, not the call's own arguments.

No grammar, or no such ancestor, is a status line message, not an error.

## Split

A newline after the open bracket, after each comma that is a direct child of
the list, and before the closer — skipping any point already at a line's end,
so a half-split list splits the rest of the way rather than gaining blank
lines. Then the affected rows are reindented by the same machinery `=` uses,
which is what makes the result look typed rather than generated.

No trailing comma is added: whether one belongs is a taste the languages
disagree on, and adding punctuation is more than the command's name promises.

## Join

Everything between the brackets onto one line: each whitespace run containing
a newline collapses to a single space — to nothing when it touches one of the
brackets — and a trailing comma left pressed against the closer comes off,
undoing the style `:tssplit` respected. Nested lists inside the span come
along; the command flattens what the cursor is in, not one level of it.

A multi-line string inside the span is flattened like everything else — the
tree could say where the strings are, but a join that silently refused parts
of its span would need a paragraph to explain. Undo is one key.

## Deliberately not here

Per-language templates (trailing commas, brace padding, argument-per-line
styles), joining constructs that need rewriting rather than reflowing
(`if`/`else` to a ternary), and a combined toggle command.
