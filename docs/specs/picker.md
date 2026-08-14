# Picker

A modal overlay that filters a list and returns a choice. Registers are its
first client — the ring is 4096 deep but only its front is reachable today, so
this is what makes that depth real.

Built as a general component rather than a register popup, because file
finding, buffer switching and `:` completion all want the same widget. The
measure of whether that succeeded: `picker.rs` must not mention registers.

## Status

Not built. Step 2 of `docs/specs/registers.md`.

## Matching: substring, not fuzzy

fzf-style subsequence matching shines on short identifier-like strings — paths,
symbols — where `sfx` should find `src/foo/index.ts`. Register entries are
prose and code blobs. When you search your clipboard you are thinking "it had
the word `retry` in it", not "these letters appear in order", and subsequence
matching against a 12-line snippet matches almost everything.

So: **case-insensitive substring**, with whitespace splitting the query into
terms that must *all* appear (in any order).

**No scoring.** Matches keep ring order, which is recency. For a clipboard that
is the right ranking and no relevance heuristic beats it — the thing you want
is usually the thing you copied recently. This is also why the picker needs no
scorer interface yet; the file picker will, and can add one then.

## Model

```rust
pub struct Item {
    /// Matched against, shown in the preview, and the row label is its first
    /// line. One field because for registers all three are the same string and
    /// duplicating a 64 MiB ring three ways is not free.
    pub text: String,
}
```

A file picker will want a preview that differs from the haystack (path vs file
contents) and must be read lazily. That is an added field then, not a reason to
carry an unused one now.

```rust
pub struct Picker {
    kind: PickerKind,
    items: Vec<Item>,
    query: String,
    /// Indices into `items`, in ring order. Recomputed on every keystroke.
    matches: Vec<usize>,
    /// Index into `matches`, not into `items`.
    selected: usize,
    /// First visible row of the list.
    scroll: usize,
    /// Whether one-character entries are shown.
    show_short: bool,
}

pub enum PickerKind {
    /// `before` mirrors `p` versus `P`.
    Register { before: bool },
}
```

`PickerKind` is a tag, not a callback. A stored closure would need `&mut Editor`
while living inside it, which fights the borrow checker for no benefit;
`Editor` matching on the kind at accept time is simpler and testable.

## Filtering

Recomputed from scratch on every keystroke. At 4096 short entries that is well
under a millisecond, so there is no incremental machinery and no worker thread.
The file picker over a large tree is where streaming becomes necessary, and
that is the moment to reach for a real match engine.

An empty query matches everything.

**Short entries are hidden by default.** Anything under two characters — which
is exactly the single-char `x` deletes — is filtered out of the list. `Ctrl-A`
toggles them back in. This is the display-side answer to `x` polluting the
ring: the data is kept, the noise is hidden.

**Selection is clamped, never lost.** When a keystroke shrinks the match list,
`selected` clamps to the new end rather than resetting to the top.

## Keys

| Key | Does |
|---|---|
| any printable | append to the query |
| `Backspace` | delete a char; on an empty query, cancel |
| `Ctrl-N` / `Down` | next match |
| `Ctrl-P` / `Up` | previous match |
| `Enter` | accept |
| `Esc` / `Ctrl-C` | cancel |
| `Ctrl-A` | show or hide short entries |

Selection **wraps** at both ends — the lists are short and wrapping is less
surprising than a dead key.

Backspace-on-empty cancelling matches how `Mode::Command` already behaves, so
the two modal line-editors stay consistent.

## Opening and accepting

`"p` and `"P` open it over the ring, carrying which paste form was asked for.

**An empty ring does not open the picker.** It sets the status to `nothing to
paste`, the same as bare `p` does — an empty overlay is a worse answer than a
message.

On accept, the chosen entry is **pushed to the ring** and then pasted. The push
is what makes `.` and a subsequent bare `p` repeat the same text instead of
whatever was most recent before, and it costs nothing to implement because
move-to-front dedupe already does exactly this. It is also the answer to "does
`.` re-open the picker" from the register spec: it does not, because the entry
is now the front and a plain paste repeats it.

On cancel, nothing changes.

## Layout

A centred floating box, list above preview, drawn over the buffer.

```
┌──────────────────────────────────────┐
│ fn main() {                          │
│   ┌──────────────────────────────┐   │
│   │ > retry                      │   │
│   │ ▸ pub fn retry(n) { for i…   │   │
│   │   // retry the request until │   │
│   │ ¶ retry_count                │   │
│   ├──────────────────────────────┤   │
│   │ pub fn retry(n) {            │   │
│   │     for i in 0..n {          │   │
│   └──────────────────────────────┘   │
└──────────────────────────────────────┘
```

Roughly 60% of the terminal in each dimension, clamped so it stays usable on
small terminals. Inside: one query line, the match list, a separator, then the
preview taking the bottom ~40%.

**Row labels** are the entry's first line with tabs expanded and the rest
elided. A `¶` prefix marks a linewise entry, so you can see before pasting
whether it will open a new line or splice inline.

**The list is viewport-bounded** like the main render pass — only visible rows
are formatted, and `scroll` follows the selection.

## Mode and actions

`Mode::Pick` is a unit variant; the state lives in `Editor.picker:
Option<Picker>`. A `Picker` is far too large to sit inside the enum the way
`Command(String)` does.

```rust
Action::OpenPicker(PickerKind)
Action::PickChar(char)
Action::PickBackspace
Action::PickNext
Action::PickPrev
Action::PickAccept
Action::PickCancel
Action::PickToggleShort
```

Verbose, but symmetric with the existing `Command*` actions, and it keeps
`input.rs` a pure key-to-action mapping with no picker logic in it.

`Mode::allows_eol()` is false for `Pick`; `label()` is `"PICK"`.

## Where the boundary sits

`picker.rs` owns state only — query, matches, selection, scroll — and exposes
what it holds. It does not draw. `ui.rs` reads `matches()`, `selected()` and
`preview()` and renders them.

This is the same split that makes the input parser testable: the entire picker
state machine can be exercised without a terminal, and if it owned ratatui
widgets the only way to test it would be to render it, which means it would not
get tested. It is also what keeps the core usable from a future non-terminal
frontend.

## Testing

`picker.rs`, no terminal:

- an empty query matches every item
- typing filters to substring matches
- multiple whitespace-separated terms must all appear, in any order
- matching is case-insensitive
- matches keep ring order rather than being scored
- next and previous move, and wrap at both ends
- selection clamps instead of resetting when the match list shrinks
- short entries are hidden by default and the toggle reveals them
- accept reports the index of the chosen item

`editor.rs`:

- `"p` opens the picker; `"P` opens it with `before` set
- `"p` on an empty ring does not open it and sets the status
- accept pastes the chosen entry, not the most recent one
- accept moves the chosen entry to the ring front, so a following `p` repeats it
- cancel leaves buffer and ring untouched
- a picked paste is one undo step

`input.rs`:

- `"p` and `"P` produce `OpenPicker` with the right `before`
- in `Mode::Pick`, printable keys become `PickChar` and the control keys map as
  tabled above
- `"_p` remains a no-op

## Deferred

A scorer interface, lazy previews, and streaming item injection — all three are
what the *file* picker needs and none of them are what the register picker
needs. Adding them now would be building for a client that does not exist yet.

Multi-select, and promoting a highlighted entry to a named register, wait for
named registers (step 3 of the register spec).
