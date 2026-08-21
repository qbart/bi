# Symbols

`:symbols` lists the declarations tree-sitter found in the buffer and jumps to
the one you pick. Module and function level: the things you navigate *to*, not
every node that happens to declare a name.

## Status

**Built.**

## Where the list comes from

A walk of the parse tree that is already there. No index, no cache, nothing to
invalidate on an edit — the tree is reparsed incrementally on every keystroke
anyway, and a symbol list derived from it is correct by construction.

Not a `symbols.scm` per grammar, which is the orthodox tree-sitter answer.
Only some of the thirty-odd grammars bi ships have a tags query at all, so
most of thirty query files would have to be written and maintained here — for
a list you fuzzy-search, where a row too many costs a keystroke.

## The rule

A node is a symbol when **its kind ends in a word that declares** and
**contains a word worth jumping to**. It needs both, and both failures are
worth remembering because each looked fine until it was run:

- **Suffix alone** (`_item`, `_definition`, `_declaration`, …) listed every
  `parameter_declaration`, `field_declaration` and `let_declaration` in the
  file — three rows per function before you reach the function — and still
  missed C3's `func_definition`.
- **Word alone** (`func`, `class`, `struct`, …) listed C3's
  `module_resolution`: the `os::` in front of every call in the file, plus
  `func_header`, `func_param_list` and `defer_stmt`.

```
declares   item  definition  declaration  specifier  spec  def
worth      func  method  class  struct  enum  interface  trait  impl
           mod  namespace  type  macro  union  constructor  def
```

`mod` covers `module` and Rust's `mod_item`. `def` as a *suffix* reaches C's
`preproc_def` without reaching `defer_stmt`, which ends in `stmt`.

## The name

The `name` field where the grammar has one, and otherwise **the first
identifier under the node**. The second route is not a fallback for exotic
grammars, it is what reaches C: a `function_definition` has a `declarator`
rather than a name, and the identifier is two levels down inside it. C3 does
the same thing through a `func_header`.

The test for an identifier is `contains("ident")`, because the grammars
disagree on the word: C says `identifier`, C3 says `ident`, Rust says
`field_identifier` and `type_identifier`. A suffix test for `ident` silently
loses every language that spells it the long way, which is most of them —
that bug shipped for about ten minutes and took C's functions with it.

## What it costs

It over-reports rather than under-reports, deliberately. Go names one type
twice — a `type_declaration` wrapping a `type_spec` — so identical adjacent
rows are collapsed by name and row; anything else it says twice, it says twice.
A row too many costs a keystroke in the filter. A row missing costs you the
feature, and you have no way to know it happened.

## The cursor lands on the name

Not on column zero of the row. The row is what you can already see; the name is
what you were looking for.

## Tests

- Rust: the `mod`, the `struct` and the `fn` — and not the field or the `let`.
- C: `add` is found, which only works through the first-identifier route.
- Choosing a row puts the cursor on the name and closes the overlay.
- A file with no declarations says so rather than opening an empty overlay,
  and a file bi has no grammar for says *that* — they are different answers.
- `:set syntax python` on a file called `script` gives it symbols, which is
  the two features meeting.
