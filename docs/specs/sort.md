# `:sort`

Lines, ordered. `:sort` over a file of imports, `:'v sort` over the block you
just selected, `:2,8sort` over the lines you counted. The range language is
[ranges.md](ranges.md) entire, which was built so a command like this one
never has to invent its own.

## Status

**Built.**

## The command

```
:[range]sort [flags]
:[range]sort!
```

| | |
|---|---|
| `:sort` | every line in the file, ascending |
| `:sort!` | the same, descending |
| `:'v sort` | the rows the selection touches |
| `:2,8sort` | lines 2 to 8 |
| `:sort u` | ascending, duplicates dropped |
| `:sort n` | by the first number on each line |
| `:sort i` | case-insensitively |

**No range is the whole file**, which is vim and is what "sort" means when
nothing narrows it. A selection or an address narrows it; there is no sorting
half a line, so a scope that is not whole rows is widened to the rows it
touches and says so — the same `whole lines` note `:m` and `:retab` give a
rectangle.

## Flags

| flag | does |
|---|---|
| `n` | compare the first number on each line; lines without one sort first |
| `i` | compare without case |
| `u` | drop lines that compare equal, keeping the first |

Flags combine in any order — `:sort un` is `:sort nu`. An unknown flag is an
error naming the flag, not a flag ignored. `n` compares numbers and nothing
else, so `i` beside it has nothing to add and is allowed to say so silently.

`!` is descending, vim's spelling. It reverses the whole ordering, `u`
included: `:sort! u` is the unique lines, largest first.

## Semantics

- **The sort is stable.** Lines that compare equal keep the order they had,
  which is what makes `:sort n` over aligned columns not scramble its ties.
- **`n` reads the first integer** on the line — an optional `-` and digits,
  wherever they first appear, so `item 12` sorts by 12. Lines with no number
  at all sort before every line with one, in the order they had.
- **`u` keeps the first** of each run of equal lines, under whatever
  comparison the other flags chose — `:sort iu` drops `Foo` when `foo` came
  first.
- **One undo step**, like every `:` command that rewrites lines.
- **The cursor lands on the first line of the sorted range**, collapsed — the
  block starts there, and the selection that named the range has been
  consumed.
- **The report counts**: `12 lines sorted`, and `, 3 duplicates dropped` when
  `u` dropped any. A range already in order says `already sorted` and touches
  nothing — no edit, no undo entry, exactly as `:retab` answers a conformant
  file.
- Sorting fewer than two lines says `nothing to sort` and changes nothing.

## Where it lives

`src/sort.rs` holds the parse and the ordering — flags in, a `Sort` out, and a
pure `sort_lines` over strings that has never heard of a buffer:

```rust
pub struct Sort {
    pub reverse: bool,     // !
    pub numeric: bool,     // n
    pub ignore_case: bool, // i
    pub unique: bool,      // u
}

pub fn parse(arg: &str, reverse: bool) -> Result<Sort, String>;
pub fn sort_lines(lines: Vec<String>, how: &Sort) -> Vec<String>;
```

The doing is `View::sort_rows`, beside `move_to` and `retab`, which are the
other commands that rearrange whole lines: resolve the scope through
[`View::region`] and `whole_rows`, read the rows, hand them to `sort_lines`,
write back what changed as one `replace_range`.

## Tests

In `sort.rs`, no buffer involved:

- bare, `n`, `i`, `u` and their combinations parse; an unknown flag is an
  error naming it.
- ordering: ascending, descending, numeric with and without numbers on every
  line, case-insensitive, stability of ties.
- `u` under each comparison, and `! u` reversing the unique lines.

In `editor.rs`:

- `:sort` orders the whole file; `:2,3sort` touches those rows and no others.
- `:'v sort` sorts the rows the selection touches, and a rectangle widens
  with the `whole lines` note.
- `:sort!` descends; `:sort n` orders `item 9` before `item 12`.
- `:sort u` drops the duplicate and the report counts it.
- one undo step; the cursor on the first line of the range.
- an ordered range says `already sorted` and adds nothing to the history.
- `:sort x` names the flag it does not have.
- `:2,99sort` says `no line 99` — the range rules are not re-implemented here.
