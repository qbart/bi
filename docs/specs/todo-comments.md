# `TODO:` and its friends

Seven words carry most of what a codebase says about itself, and all seven read
as ordinary comment-grey:

```
TODO:  HACK:  WARN:  PERF:  NOTE:  TEST:  FIX:
```

Colouring them is a two-line provider now that decorations exist, and the
result is that `FIX:` stops hiding in the same shade as the sentence explaining
what the function does.

## Status

**Built.**

## What is matched

An **uppercase** keyword, on a **word boundary**, followed by a **colon**.

```
TODO: rewrite this           matches
TODO(bart): rewrite this     matches — the owner in parens is a convention
todo: rewrite this           does not match
MYTODO: something            does not match — a boundary is required
TODO rewrite this            does not match — the colon is the marker
```

Uppercase only, because the word is a marker rather than a word: `todo` in a
sentence about a to-do list is prose, and highlighting it would be a bug. The
colon is required for the same reason, and it is what everybody already types.

Aliases, because the same thought has more than one spelling in the wild and
splitting them would mean two colours for one meaning:

| Colour | Keywords |
|---|---|
| fix | `FIX` `FIXME` `BUG` `ISSUE` |
| todo | `TODO` |
| warn | `WARN` `WARNING` `XXX` |
| perf | `PERF` `OPTIM` `PERFORMANCE` |
| note | `NOTE` `INFO` `TEST` `TESTING` |

`TEST:` shares `NOTE:`'s colour rather than getting a sixth: both mean "read
this", neither means "something is wrong", and a palette with six shades of
attention has no shades left for anything else.

## Anywhere, not only in comments

bi does not check that the match is inside a comment, and that is a decision
rather than an omission. It has the parse tree and could: the cost is that a
`TODO:` in a Markdown list, a YAML file, a commit message or any file whose
grammar bi does not ship stops being highlighted, and those are exactly the
places people write them. The false positives the other way — `NOTE:` inside a
string that meant nothing by it — are cosmetic and rare.

One rule, no per-language surprises, and the same behaviour in a file with a
grammar and a file without one.

## The colours

Five new `[ui]` keys, so a theme decides:

```toml
todo_fix   = { fg = "#282828", bg = "#fb4934", bold = true }
todo_todo  = { fg = "#282828", bg = "#83a598", bold = true }
todo_warn  = { fg = "#282828", bg = "#fabd2f", bold = true }
todo_perf  = { fg = "#282828", bg = "#d3869b", bold = true }
todo_note  = { fg = "#282828", bg = "#b8bb26", bold = true }
```

Required keys, like every other `[ui]` entry, so a theme cannot ship with a
hole where one of them should be — `Ui::REQUIRED` is what enforces that and a
test walks it against every built-in.

Only the keyword and its colon are painted. Colouring the rest of the line
after them, which some editors do, makes the *comment* loud when what wanted
to be loud was the marker.

```
[options]
todo_comments = true
```

Off turns the provider off entirely — no scan, no decorations.

## How it works

A `Repaint` decoration per match (`docs/specs/decorations.md`), over the
keyword's char range, on the `Under` layer so a selected line still reads as
selected. The scan is per visible row, on the row's own text, which keeps it
bounded by the screen like every other provider.

## Tests

- Each keyword and each alias resolves to its own colour.
- Lowercase does not match, a keyword without its colon does not match, and a
  keyword glued to a longer word does not match.
- `TODO(name):` matches, because the owner-in-parens convention is everywhere.
- Two on one line both match.
- The range covers the keyword, the owner in parentheses if there is one, and
  the colon — and nothing after it.
- A longer keyword beats the shorter one inside it: `WARNING:` is not `WARN`
  followed by rubbish.
- Off produces nothing at all.
