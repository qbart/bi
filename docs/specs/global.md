# `:g` — a command on every matching line

`:g/TODO/d` deletes every line with a TODO on it. `:v/save/d` deletes every
line without `save`. `:g/^fn /normal A {` appends to every function header.
One scan, one command, one undo step — the batch edit vim inherited from `ed`
and the reason `grep` is called grep.

## Status

**Built.** `:g`, `:v`/`:g!`, `:normal` (alone and under a range), and `:d` —
the line delete `:g` made necessary. The pattern language is the literal
smartcase one `/` and `:s` share; regex lands under all of them at once.

## The commands

```
:[range]g/{pattern}/{cmd}
:[range]g!/{pattern}/{cmd}
:[range]v/{pattern}/{cmd}
:[range]normal {keys}
:[range]d
```

| | |
|---|---|
| `:g/foo/d` | delete every line containing `foo` |
| `:v/foo/d` | delete every line *not* containing `foo` |
| `:g!/foo/d` | the same — `v` is `g!` spelled with vim's other name |
| `:g/TODO/s/old/new/g` | substitute, but only on TODO lines |
| `:g/^use /m 0` | herd the imports to the top |
| `:g//d` | the pattern is the last thing you searched for |
| `:2,20g/foo/d` | only lines 2 to 20 are scanned |
| `:g/fixme/normal A !` | append ` !` to every fixme line |
| `:%normal I// ` | comment out every line, no pattern needed |
| `:d` | delete the cursor's line |
| `:2,5d` | delete lines 2 to 5 |

**No range means the whole file** for `:g` and `:v`, which is vim and is what
"global" means. `:normal` and `:d` without a range take the cursor's line,
like `:s`.

## `:g` — what it does

1. Resolve the range to rows — the whole file when nothing narrows it.
2. Scan once, up front: every row whose text contains the pattern is marked.
   `:v` (and `:g!`) marks the rows that do not. The scan finishes before the
   first command runs, so a command cannot edit a line into or out of the
   match set — `:g/a/normal olala` does not chase its own output.
3. For each marked row, top to bottom: put the cursor on it, run the command.
4. One undo step for the whole thing, however many lines it touched.
5. The report is `{n} matching lines`; no matches is `pattern not found:
   {pattern}` and nothing runs.

A command that deletes or adds lines shifts every row below it, so the walk
carries the difference: each marked row is adjusted by how much the file has
grown or shrunk above it. That is exact for every command that stays on its
own line — which the allowed ones do — where vim gets the same answer with
marks.

**The command is one of `d`, `s`, `&`/`&&`, `m`/`move`, `case`, `retab`,
`normal`.** These are the commands that act on the cursor's line when no range
narrows them, which is exactly the contract the walk needs. Anything else —
`:e`, `:q`, a window command, another `:g` — is refused by name:
`` `:g` runs d, s, &, m, case, retab or normal — not `q` ``. A whitelist
rather than a blacklist, because the failure mode of a blacklist is `:g/x/q`
closing the editor on the first match.

An empty command is an error — vim prints the line, bi has no `:print` and a
`:g` that silently does nothing is worse than one that asks.

The pattern is delimited like `:s` — any of the same delimiter characters, so
`:g#/usr#d` works on paths — and an unclosed pattern is fine: `:g/foo` is
`:g/foo/` is "and do what?", the empty-command error.

**The pattern becomes the last search**, exactly as `:s`'s does. That is what
makes `:g/foo/s//bar/g` the idiom it is in vim: the inner `s` names no
pattern and gets `foo`, and `n` afterwards walks what the whole thing
touched.

## `:normal` — typed keys, replayed

`:normal {keys}` (also `:norm`) feeds the argument through the same
key-to-command machinery a frontend uses, one character per key, against a
fresh keymap state. What `A // fixme<Esc>` would have done, `:normal A //
fixme` does — the trailing `Esc` is pressed for you: whatever mode the keys
end in, the editor is back in normal mode when the command is done, exactly
so a half-finished insert cannot leak into the next line of a `:g`.

- **Under a range**, the keys run once per row, cursor first placed at column
  0 of that row — `:%normal I// ` comments every line.
- **One undo step** per `:normal`, range and all.
- **The plain keymap**, not the user's remaps — vim's `:normal!`, which is
  the one every script means. The unbanged name keeps the short spelling;
  when remap-respecting replay is wanted it can take a flag.
- **Raw characters only.** There is no `<Esc>` notation yet; the keys are
  what you could type on a `:` line, which insert-mode edits and normal-mode
  operators almost always are. The notation is deferred, not refused —
  `config::spell` already names keys and can be taught to read them back.
- **It does not nest.** `:normal :g/x/normal j` is refused: replayed keys
  running the replayer is a loop with a keyboard in it. Depth one, error
  `normal does not nest`.

The argument is taken as written, trimmed at the edges like every ex
argument; `:normal` with nothing after it is `normal what?`.

## `:d` — delete lines

`:[range]d` deletes the rows the range names, the cursor's line by default.
The cursor lands at column 0 of the line that moved up into the gap, clamped
to the new end of the file.

Short name only. `:delete` keeps meaning what the tree taught it — delete a
*path* — and the two do not meet: `:delete` demands an argument and `:d`
refuses one, so neither can be mistyped into the other. `:d` captures nothing:
a delete you can paste back is `dd`, and a `:g/foo/d` that pushed four hundred
lines through the register ring would have buried what was there.

## Where it lives

- Parsing: `parse_ex` grows `g`/`v` (with a glued splitter, the same one `:s`
  has) and `d`, `normal`. The sub-command of a `:g` stays a string until it
  runs — it is parsed fresh on each marked line by the same `parse_ex`, so
  there is exactly one ex grammar.
- Running: `Editor::run_global` and `Editor::run_normal_keys`, beside
  `run_ex` — they need the whole editor, not a view, because the sub-command
  dispatch does.
- The undo group: `Buffer::begin_undo_group` / `end_undo_group` defer the
  per-command commits into one revision — the history tree stays append-only,
  the group is just a commit that waited.
- `:normal` drives `input::Input` — the core already owns the key grammar;
  an embedder's frontend and `:normal` now read from the same table.

## Tests

In `editor.rs`:

- `:g/foo/d` deletes exactly the matching lines, top one included, bottom one
  included, and is one undo step.
- `:v/foo/d` keeps exactly the matching lines.
- `:g!/foo/d` is `:v/foo/d`.
- `:2,3g/foo/d` scans only those rows.
- `:g/foo/s//bar/` rewrites the `foo`s on matching lines — the `:g` pattern
  is the last search by the time the inner `s` asks.
- `:g/a/normal ox` does not run on the lines it just made.
- `:g/foo/q` is refused by name and nothing ran.
- no match: `pattern not found`, nothing changed.
- `:g/foo/` — and do what?
- `:normal A!` appends to the cursor's line and returns to normal mode.
- `:%normal I// ` comments every line, one undo step.
- `:normal :normal x` does not nest.
- `:d` deletes the cursor's line; `:2,5d` deletes four; both one undo step.
- `:d 4` is an error — `:d` refuses an argument.
