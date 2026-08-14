# Registers

Yank, delete and change put text somewhere; paste takes it back out. Vim gives
you 36 addressable slots and expects you to choose one *at yank time*, which is
the wrong moment — you rarely know yet whether a thing is worth keeping. bee
keeps everything automatically in a deep ring and moves the choice to paste
time, where a fuzzy picker can search it.

## Status

Step 1 is **built**. Steps 2–4 are recorded here because their decisions shape
step 1's data model, not because they are being built yet.

| Step | Scope | Needs |
|---|---|---|
| **1** ✅ | Ring, auto-capture on `y`/`d`/`c`/`x`, `p`/`P`, `"_` | nothing new |
| 2 | Fuzzy picker over the ring, `"p` / `"P` | a general overlay UI |
| 3 | Named registers, `"n` | step 2 + a name prompt |
| 4 | System clipboard, `"+` / `"*` | a clipboard backend |

## Model

An entry is text plus how it was taken. The *how* is what makes `yy` then `p`
open a new line while `yw` then `p` splices inline, so it travels with the text
rather than being decided at paste time.

```rust
pub struct Entry {
    pub text: String,
    pub kind: EntryKind,
}

pub enum EntryKind {
    /// Taken as a span within a line. Pastes inline.
    Charwise,
    /// Taken as whole lines. Pastes as new lines.
    Linewise,
}
```

`kind` is `Linewise` exactly when the motion that produced it was linewise —
`Motion::kind() == Kind::Linewise`. No blockwise: there is no visual block mode
to produce it.

## The ring

```rust
pub struct Registers {
    ring: VecDeque<Entry>,   // front is most recent
    capacity: usize,         // default 4096
    byte_budget: usize,      // default 64 MiB
}
```

**Capacity: 4096 entries.** Configurable later. Vim's nine numbered slots are
why it needed a separate small-delete register — three `x` presses would evict
real history. At 4096 that pressure is gone, so every capture goes on the ring
and nothing is diverted.

**Dedupe by move-to-front.** Pushing text that already exists in the ring
removes the old copy first. Re-yanking the same thing bumps it up rather than
occupying a second slot. Comparison is on `text` and `kind` together, so the
same text taken charwise and linewise are distinct entries — they paste
differently, so they are different things.

**Byte budget: 64 MiB total.** After a push, evict from the back until the ring
fits. Entries are never truncated; a clipboard that silently corrupts what you
copied is worse than one that forgets. A single entry larger than the whole
budget is still kept — it is the most recent — and evicts everything else.

**Eviction order** is always from the back: capacity first, then budget.

## What gets captured

Every operator that removes or copies text pushes one entry, covering exactly
the range it acted on:

| Keys | Pushes |
|---|---|
| `y{motion}`, `yy`, `Y` | the yanked range |
| `d{motion}`, `dd` | the deleted range |
| `c{motion}`, `cc` | the replaced range |
| `x` | the deleted char |

One command is one entry, so `3dd` pushes a single three-line entry rather than
three. This matches the undo grouping rule already in `Editor::apply`.

Nothing is pushed when the range is empty — `b` at the start of the buffer
changes no text and must not put an empty string on the ring.

`x` pushing single characters is deliberate. They are noise in a picker, but
that is a *display* problem solved in step 2 by hiding short entries behind a
toggle, not a storage problem solved by dropping data.

`x` is `Operate { op: Delete, motion: Right, count }`, which is what it has
always been — `Motion::Right` already stops at the line end, so `5x` clamps
there exactly as before. That deleted a special case rather than teaching it
about registers; `Buffer::delete_char_forward` is gone.

### The black hole

`"_` before an operator captures nothing: `"_dd` deletes a line without
touching the ring. This is the escape hatch for throwing away a large junk
block without pushing 4096 useful entries one step closer to the exit.

`"_p` is a no-op, matching Vim.

## Paste

`p` and `P` read the **front** of the ring. There is no separate "last yank"
slot: after yanking X then deleting Y, plain `p` gives you Y, exactly as Vim's
unnamed register does. Recovering X is what the picker is for — it replaces
Vim's `"0` idiom rather than reproducing it.

| Kind | `p` | `P` |
|---|---|---|
| Charwise | insert after the cursor char | insert at the cursor |
| Linewise | open a new line below | open a new line above |

**Cursor lands** on the last char of pasted charwise text, or on the first char
of the first pasted line for linewise. (Vim uses first *non-blank* for
linewise; bee has no first-non-blank motion yet — `^` is currently an alias for
`0` — so first char is the honest approximation, and it moves when `^` becomes
real.)

**Counts repeat the content**: `3p` pastes three copies as one edit and one
undo step.

**An empty ring** sets the status line to `nothing to paste` and changes
nothing.

**`.` repeats the paste**, not the choice. Once step 2 lands, repeating a
picker-paste re-pastes the same entry rather than re-opening the picker — a
repeat that stops to ask a question is not a repeat.

## Grammar

`"` introduces a register reference and takes exactly one key of lookahead. The
key sets do not overlap, so the parse is unambiguous:

```
"_{operator}     black hole            step 1
"p  "P           the ring picker       step 2
"n{operator}     named register        step 3
"+  "*           system clipboard      step 4
```

Digits and letters `a`–`z` are **not** register names — that namespace is gone
along with `"5p` and `"ap`. A count therefore goes before the quote: `3"p`.
`"3p` is rejected rather than accepted as a count, because it reads like a Vim
register and would mislead.

Keys after `"` that name nothing cancel the whole command, the same way `dz`
cancels a pending operator today.

## Where the state lives

`Registers` belongs to `Editor`, not to `Buffer`. Registers are global: yanking
in one file and pasting in another is the point, so they must outlive any
single buffer. This matters now rather than later, because putting them on
`Buffer` would have to be undone the moment a second buffer exists.

`Buffer::operate` stays register-agnostic. It returns what it removed:

```rust
pub fn operate(&mut self, op: Operator, motion: Motion, count: usize) -> Option<Entry>
```

`Editor` decides whether that entry reaches the ring, which is what makes the
black hole a caller-side policy rather than a flag threaded through the buffer.
Paste is the mirror: `Buffer::paste(&mut self, entry: &Entry, before: bool,
count: usize)` takes an entry and knows nothing about where it came from.

## New actions

```rust
Action::Paste { before: bool, count: usize }
Action::Operate { op, motion, count, sink: Sink }
```

`Paste` carries its own count rather than repeating through `Editor::apply`, so
three copies are one edit and one undo step. `Sink` is `Ring` or `BlackHole`, and grows a `Named(String)` and
`Clipboard` variant later. An enum rather than a `bool` specifically so those
additions don't rewrite call sites.

## Undo

Paste is an ordinary edit through `Buffer::apply_edit`, so it emits an `Edit`
for tree-sitter and lands in the undo tree as one step per command — both fall
out of the existing rules with nothing new to add.

Undo does **not** roll back the ring. Undoing a delete puts the text back in
the buffer and leaves the entry on the ring, matching Vim.

## Testing

Ring, in `registers.rs`, with no buffer involved:

- push then read front
- move-to-front dedupe: pushing an existing entry does not grow the ring
- same text, different `kind`, are two entries
- capacity eviction drops the oldest
- byte-budget eviction drops the oldest
- an entry bigger than the whole budget survives alone

Capture and paste, in `buffer.rs` / `editor.rs`:

- `dw` yields a charwise entry, `dd` a linewise one
- `3dd` pushes one entry, not three
- an empty range pushes nothing
- charwise `p` inserts after the cursor, `P` at it
- linewise `p` opens a line below, `P` above
- `3p` pastes three copies in one undo step
- `"_dd` deletes and pushes nothing
- paste on an empty ring reports and changes nothing
- cursor position after each paste form

Parser, in `input.rs`:

- `"_` reaches the operator with `Sink::BlackHole`
- `"` followed by a non-name cancels
- `3"p` parses its count; `"3p` is rejected

## Deferred, with the decisions already made

**Step 2 — picker.** Build the overlay as a general fuzzy-list component, not a
register-specific popup; file finding, buffer switching and `:` completion all
want the same thing, and `nucleo` is the matcher named in RECOMMENDATION.md.
The picker hides entries below a length threshold by default with a key to
reveal them, and offers promoting a highlighted entry to a named register —
which is what removes the last reason to name anything at yank time.

**Step 3 — named registers.** `"n{operator}{motion}` yanks, then prompts for a
name *after* the text is captured, reusing the `:` command-line machinery.
`"np` fuzzy-picks among names. Named registers are a separate space from the
ring; they are the "I will paste this eleven times" case that a search-based
workflow serves badly.

**Step 4 — clipboard.** `"+` is CLIPBOARD, `"*` is PRIMARY, matching Vim rather
than swapping them — `"+y` is too ingrained to redefine. Both collapse to the
one system clipboard on Windows and macOS. Needs a backend decision: `arboard`
requires a display server, OSC 52 works over SSH, and a terminal editor
probably wants both.
