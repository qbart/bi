# Command-line history

Every `:` line you run is remembered, and `Ctrl-R` on the `:` line opens the
picker over them. Enter puts the chosen line back on the command line **without
running it**, so the thing you reach for a history for — a long command with one
word wrong — is one keystroke from being fixed rather than retyped.

Vim spells this `q:`, a whole window in a mode of its own, and `Up`/`Down`,
which walks a list you cannot see. There is already a fuzzy overlay here, built
as a general widget for exactly this reason; the history is its third client.

## Status

**Built.** Session-only, `:` lines only. What was deliberately left out is at
the bottom, with the reasons.

## The store

```rust
pub struct History {
    lines: Vec<String>,   // front is most recent
    capacity: usize,      // 200
}
```

Its own module, `src/cmd_history.rs`. Not `history.rs`, which is the undo tree —
the two share a word and nothing else, and putting them together would make both
harder to read.

**Newest first**, matching the register ring: recency is the ranking, the picker
keeps the order it is given, and the line you want is nearly always one of the
last few.

**Dedupe by move-to-front.** Running a line that is already in the list removes
the old copy first. Running `w` forty times leaves one entry, not forty, which
is what keeps the list worth searching.

**Blank lines are not recorded.** `:` then `Enter` is a line that does nothing,
and a history full of empty rows is a history you scroll past.

**Capacity 200.** Enough that nothing you would go looking for falls off, small
enough that the whole list stays scannable. Eviction is from the back.

## What gets recorded

Exactly the lines you typed on the `:` line and pressed Enter on —
`Action::CommandExecute`, and nothing else.

Not `Action::Ex { run: true }`. That is a config keybinding or an internal
caller reaching the same ex parser, and a history of commands you never typed is
noise in the one list that exists to give you back your own keystrokes. Vim
does not record mappings either.

**A line is recorded before it runs**, so a command that failed is in the
history. Recalling and fixing a typo is most of the point; a history that keeps
only what worked is missing the case it is for.

## The key

`Ctrl-R` on the `:` line opens the picker over the history.

Once it is up, it is the picker every other client gets, with no new keys to
learn:

| Key | Does |
|---|---|
| `Ctrl-R` | opens it, from the `:` line |
| typing | filters, on the same all-terms-must-appear match as everywhere else |
| `Ctrl-N` / `Ctrl-P`, `Down` / `Up` | move the highlight, wrapping |
| `Left` / `Right`, `Home` / `End` | move the query's cursor — see [picker-cursor.md](picker-cursor.md) |
| `Enter` | puts the line on the `:` line, unrun |
| `Esc` / `Ctrl-C` | back to the `:` line as you left it |
| `Backspace` | deletes a query character; on an empty query, cancels |

`Ctrl-R` is vim's register-insert key on its command line, which bi does not
have. It is worth taking: it is the key fingers already reach for at a prompt
that wants a previous thing, and the shells reinforce it — `Ctrl-R` in bash and
zsh is *exactly* this feature, an incremental search backwards through what you
ran.

**What you have already typed seeds the query.** `:w` then `Ctrl-R` opens with
`w` in the query and the list already narrowed. The half-typed line is what you
know about the command you want, so throwing it away and asking you to type it
again in the query is a keystroke charged for nothing.

**Enter replaces the whole line**, and does not run it. It never appends: the
entries are whole commands, and splicing one onto the end of another produces a
line that was never a command.

**Esc restores the line exactly**, seeded query edits and all. Cancelling is how
you say the history was not what you wanted, and losing your half-typed command
to it would make `Ctrl-R` a key you hesitate over.

**An empty history reports and stays put** — `no command history` in the status,
still on the `:` line. An empty overlay is a worse answer than saying so, which
is the rule `nothing to paste` already follows.

## Where the picker had to give

Two changes, both in the widget rather than the client.

**The short-entry filter becomes a per-picker length, not a constant.**
`MIN_LEN = 2` exists because single-character `x` deletes would bury the
register list. On a command history it hides `w`, `q` and `x` — the shortest
commands there are, typed the most often, and the ones you would most want back.
Registers keep 2; history and the buffer list pass 0.

The buffer list passing 0 is a fix, not a change of heart: a file named `a` was
being hidden from `:ls` behind `Ctrl-A`, which nobody designed and no test
caught.

**No preview pane for history.** The preview exists to show a register entry
that is longer than its row. A command line is one line and is already the row,
so the pane would repeat it and take a third of the overlay to do it. The rows
get the space. The file and buffer lists went the same way for the same reason
(`docs/specs/buffers.md`), which leaves the register ring as the only kind that
previews at all.

## Where the state lives

`History` belongs to `Session`, beside the registers, for the same reason they
do: it is not a fact about any buffer, and it has to survive every one of them
being closed. Public, so a frontend can show it or seed it — an embedder that
wants a history from somewhere else can fill it, and nothing in the core reads
it except the picker.

**The mode the picker returns to becomes `Session::pick_from: Option<Mode>`**,
replacing `pick_over: Option<VisualKind>`. That field existed to give a visual
selection back when a register pick was cancelled; the history needs the same
thing for a half-typed `:` line. Storing the mode itself makes "the picker
returns you where you were" one rule instead of one rule and a special case, and
the register and buffer paths keep the behaviour they had.

## New shape

```rust
PickerKind::History          // in picker.rs, beside Register and Buffer
Picker::set_query(&mut self, query: String)
Picker::new(kind, items, min_len)
History::push(&mut self, line: &str)
History::lines(&self) -> &[String]
```

`Ctrl-R` maps to the existing `Action::OpenPicker(PickerKind::History)`. The
half-typed line is not on the action: `input.rs` cannot see it, and the editor
already holds it in `Mode::Command`. One ordering fix goes with it — the `:`
line's `KeyCode::Char(c)` arm currently swallows `Ctrl-R` and types a literal
`r`, so the ctrl arm has to sit above it.

## Testing

The store, in `cmd_history.rs`, with no editor involved:

- a pushed line is at the front
- re-running an existing line moves it to the front and does not grow the list
- a blank or whitespace-only line is not recorded
- capacity eviction drops the oldest

The editor, in `editor.rs`:

- executing a `:` line records it; `Action::Ex { run: true }` does not
- a command that failed is still in the history
- `Ctrl-R` with a half-typed line opens the picker with it as the query
- accepting puts the line on the `:` line and runs nothing
- cancelling restores the half-typed line
- an empty history reports and leaves you on the `:` line

The keymap, in `input.rs`:

- `Ctrl-R` on the `:` line opens the history picker rather than typing `r`

## Deferred, with the decisions already made

**Search history.** `/` and `?` want the same key over their own list. A second
`History` on the session and a second `PickerKind`, not a shared list: a regex
offered on a `:` line is noise, and the two prompts have different vocabularies.
Nothing in the store needs to change for it.

**Persistence.** A history that survives restarting is a state file — where it
lives per platform, what caps it, and what two instances do when both write.
That is a subsystem, and the session-only list is worth having before it. When
it comes, it fills `History` at startup and reads it at exit; nothing else moves.

**`Up`/`Down` on the `:` line** — no longer deferred. It is built, over this
same store, and [cmdline.md](cmdline.md) has the rules. The argument above
still holds and is why it stayed small: no prefix filtering, no wrapping, and
`Ctrl-R` remains the way to *find* a line. `Up` is for the last one, or the one
before it, which is a different job and a real one.
