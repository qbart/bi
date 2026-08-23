# `:s` — substitute

`:%s/2024/2025/g` is the command everyone types first and the one bi did not
have. `search.md` deferred it alongside regular expressions, on the grounds
that they arrive together; they do not. The pattern language is one question
and "replace this across these lines" is another, and the second one is worth
answering on its own — most substitutions anyone runs are a word for a word.

So: `:s` now, literal patterns, and the regex goes in underneath it later
without the command changing shape.

## Status

**Built.** Literal patterns, any delimiter, a range, `g`, `i`/`I` and `n`,
and `&`/`g&`/`:&&` repeating the last one. `c` (confirm) and the whole of the
pattern language are deferred, with reasons at the bottom.

## The command

```
:[range]s/{pattern}/{replacement}/[flags]
```

| | |
|---|---|
| `:s/a/b/` | the first `a` on the cursor's line |
| `:s/a/b/g` | every `a` on it |
| `:%s/a/b/g` | every `a` in the file |
| `:2,5s/a/b/g` | every `a` in lines 2 to 5 |
| `:'v s/a/b/g` | every `a` in what is selected — a rectangle's columns included |
| `:'<,'>s/a/b/g` | every `a` in the rows the selection touches |
| `:s#/usr#/opt#` | any delimiter, so a path needs no escaping |
| `:%s//new/g` | the pattern is the last thing you searched for |

The range language is [ranges.md](ranges.md) entire, which was built for this.
**No range means the cursor's line**, which is vim and is why `%` is the most
typed character in the command.

`:s` walks a [region](regions.md) a row at a time, and "the first match on a
line" means the first match *in that row's span* — which is what makes
`:'v s/a/b/` inside a rectangle stay inside the columns instead of reaching out
to the rest of the line.

## Flags

| flag | does |
|---|---|
| `g` | every match on a line, not just the first |
| `i` | match without case |
| `I` | match with case |
| `n` | count the matches and change nothing |

**Without `i` or `I` it is smartcase**, exactly as `/` is: an all-lowercase
pattern ignores case, and one uppercase character in it makes the whole thing
case-sensitive (`search.md`). The two flags are there for when that guess is
wrong, which is the only reason vim's `\c` and `\C` exist.

`n` prints what `:s` would have said and leaves the buffer alone. It is three
lines of code and it is the honest way to answer "how many are there" without
running the thing and pressing `u`.

An unknown flag is an error naming the flag, not a flag ignored.

## The delimiter

**Whatever character follows `s`**, as long as it is not alphanumeric, not
whitespace, and not one of `\`, `"` or `|` — vim's rule. `:s#a#b#` and
`:s,a,b,` are the same command as `:s/a/b/`, and a path or a URL in the pattern
needs no escaping at all.

The closing delimiter is optional: `:%s/old/new` is `:%s/old/new/`. Vim allows
it and everybody uses it.

`s` glued to its argument needs the same trick `:m` needed — `:%s/a/b/` splits
on whitespace into a command called `s/a/b/`, and the error message would say
so. The name ends at the first character that cannot be part of one, which is
the delimiter. That rule is what keeps `:set`, `:sp` and `:split` themselves:
their next character is a letter, and a letter is never a delimiter.

## Escaping

Inside the pattern and the replacement, `\` followed by the delimiter is that
character, and `\\` is a backslash. Nothing else is unescaped: `\d` is a
backslash and a `d`, and matches exactly that.

That is not a placeholder for a regex — it is what "literal" has to mean if
the command is to be honest. When the pattern language lands, `\d` starts
meaning a digit, and every line written before then that contained a literal
`\d` was already saying something a regex would have read differently. The
delimiter escape is the only one that has to exist, because without it there
are patterns you cannot type at all.

**No `&`, no `\1`, no `\r`.** `&` in a vim replacement is the whole match,
which with a literal pattern is a longer way of writing what you already typed;
`\1` needs groups; `\r` splits a line, which is a different operation wearing
this one's clothes. All three arrive with the regex, and taking `&` now would
mean every replacement containing an ampersand needed escaping for a feature
nobody could use yet.

## What it does

1. Resolve the range to rows, or say why not — `no line 99` comes from
   `ranges.md` and is already written.
2. For each row, find the matches inside it. First only, or all of them with
   `g`.
3. Apply them **back to front**, so an earlier replacement cannot move a later
   one's offsets.
4. One undo step for the whole command. `:%s/a/b/g` across four hundred lines
   is one thing you did and one `u` undoes it.
5. The cursor lands on the **first column of the last line changed**, which is
   vim.
6. The pattern becomes the last search, so `n` walks what you just replaced.

**The report is vim's**: `3 substitutions on 2 lines`, singular where it
should be. Nothing matched says `pattern not found: old` and changes nothing —
an error, because a `:s` that quietly does nothing is one you assume worked.

**A match is not found inside a previous replacement.** Matching happens once,
per line, before anything is written. `:%s/a/aa/g` doubles each `a` and stops;
it does not chase its own output.

## Where it lives

`src/substitute.rs` holds the parse — a string in, a `Substitute` out, no
buffer and no editor:

```rust
pub struct Substitute {
    pub pattern: String,
    pub replacement: String,
    pub all: bool,            // g
    pub case: Option<bool>,   // i / I; None is smartcase
    pub count_only: bool,     // n
}

pub fn parse(arg: &str) -> Result<Substitute, String>;
```

The command name and the delimiter are the caller's; `parse` is handed
everything after `s`. It is a pure function with its own tests because that is
where every rule above except "back to front" lives.

The doing is `View::substitute`, beside `move_to` and `recase`, which are the
other two `:` commands that rewrite lines.

`Buffer::matches_in_cased` is `matches_in` with the case rule handed in rather
than derived from the pattern. `matches_in` keeps its signature and delegates
with `None`, so the search highlight and the match count are untouched.

## `&` — again

A substitute that worked once is usually about to be wanted again, a line at a
time, wherever the cursor has got to since.

```
:[range]&        the last substitute, over the range
:[range]&&       the same command — one spelling, written twice
&                the ex command, on the cursor's line
g&               the ex command, over the whole file
```

**`&` repeats the last `:s` exactly — flags included.** Vim's `:&` drops the
flags and `:&&` keeps them, a distinction almost nobody wants and everybody
trips over; bi spells both the same on purpose, so there is one thing to
remember instead of two. `g&` is `:%&&` and nothing more — vim's `g&` also
swaps in the last *search* pattern, which turns "do that again everywhere"
into "do something related everywhere", and bi declines the swap.

What is remembered is the command as it ran: the pattern already resolved, so
`:s//b/` then `&` repeats the search that was in force *then*, not whatever
has been searched since. `n` — count only — is remembered too, because
repeating a question is still repeating. A `:s` that failed to parse or
matched nothing leaves the memory alone; `&` before any `:s` at all says
`no substitute to repeat`.

The keys are the ex command spelled for the keyboard: `&` is
`Action::Ex { line: "&&" }`, `g&` is `Action::Ex { line: "%&&" }` — the same
shape `ga` and `gd` already have, so the keymap learns no new machinery.

The memory is `session.last_substitute`, an `Option<Substitute>` set beside
`last_search` where the substitute already reports its pattern.

## Deferred

**Regular expressions.** The reason `search.md` deferred `:s` in the first
place, still the biggest missing thing here, and now the only one. When they
land they land in `Buffer::matches_in_cased` and every caller — `/`, `n`, the
highlight, this — gets them at once. `\1` and `\r` come with them.

**`c`, confirm.** `:%s/a/b/gc` stops on each match and waits for `y`, `n`, `a`,
`q` or `l`. That is a modal loop with its own keymap and its own drawing, which
is a feature rather than a flag, and it wants the match highlighted on screen
while it asks. Nothing here blocks it.

## Tests

In `substitute.rs`, no buffer involved:

- `/a/b/` and `#a#b#` parse the same, and `,` works too.
- a missing closing delimiter is allowed; a missing separator is not.
- `\/` in the pattern is a slash and does not end the field; `\\` is a
  backslash; `\d` stays two characters.
- flags parse in any order and an unknown one is an error naming it.
- an empty pattern parses, and means "the last search" to the caller.
- an alphanumeric delimiter is refused, which is what keeps `:set` a command.

In `editor.rs`:

- `:%s/2024/2025/g` — the line that started this — rewrites every occurrence.
- no range touches the cursor's line only; `:2,3s/…` touches those two.
- without `g` only the first match on each line goes.
- back-to-front: two matches on one line both land, and the second is not
  shifted by the first.
- the whole command is one undo step.
- `i` matches what smartcase would not, `I` refuses what it would.
- `n` reports the count and leaves the text alone.
- nothing matched is an error and no edit.
- the cursor ends on the last line changed, and `n` afterwards finds the
  pattern.
- `:2,99s/…` says `no line 99` — the range rules are not re-implemented here.
- `&` repeats the last substitute on the cursor's line, flags and all.
- `g&` repeats it over the whole file.
- `&` before any substitute says so and changes nothing.
- the memory holds the pattern that *ran*: `:s//b/` then a new search then
  `&` repeats the old pattern, not the new one.
