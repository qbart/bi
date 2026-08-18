# Registers

Yank, delete and change put text somewhere; paste takes it back out. Vim gives
you 36 addressable slots and expects you to choose one *at yank time*, which is
the wrong moment — you rarely know yet whether a thing is worth keeping. bi
keeps everything automatically in a deep ring and moves the choice to paste
time, where a fuzzy picker can search it.

## Status

Steps 1 and 2 are **built**. Steps 3–4 are recorded here because their
decisions shape the data model, not because they are being built yet.

The picker matches by **substring**, not fuzzily: register entries are prose and
code, where "these letters appear in order" matches nearly everything. Matches
keep ring order, so recency is the ranking.

| Step | Scope | Needs |
|---|---|---|
| **1** ✅ | Ring, auto-capture on `y`/`d`/`c`/`x`, `p`/`P`, `"_` | nothing new |
| **2** ✅ | Picker over the ring, `"p` / `"P` | a general overlay UI |
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

**Cursor lands** on the last char of pasted charwise text, or on the first
*non-blank* of the first pasted line for linewise — vim's rule, and it matters
the moment you paste indented code. This waited on step 3 of `motions.md`:
until `^` was a real first-non-blank motion rather than an alias for `0`,
first-char was the honest approximation. `scripts/vim_differential.py` now
pins it.

**Counts repeat the content**: `3p` pastes three copies as one edit and one
undo step.

**An empty ring** sets the status line to `nothing to paste` and changes
nothing.

**`.` repeats the paste**, not the choice. Once step 2 lands, repeating a
picker-paste re-pastes the same entry rather than re-opening the picker — a
repeat that stops to ask a question is not a repeat.

## Paste over a selection

In visual mode `p` **replaces the selection** rather than inserting beside it.
Select, paste, and what you selected is gone — vim's `v_p`, and the reason
`viwp` is how you overwrite a word.

This is one action, not a delete followed by a paste. Both halves have to see
the same register: the text taken out lands on the ring, and reading the
register afterwards would hand back what was just removed instead of what was
asked for. So the entry is read first, the range is replaced in a single edit,
and only then is the removed text captured.

**The removed text goes on the ring for `p`, nowhere for `P`.** That is the
whole difference between them here — "before/after the cursor" means nothing
when the selection says exactly where the text goes. `p` therefore *swaps*:
select A, `p` over B, and B is now the front of the ring ready to paste
somewhere else. `P` leaves the ring alone, which is what you want when pasting
the same thing over several selections in turn.

| Spelling | Pastes | Removed text |
|---|---|---|
| `p` | ring front | pushed on the ring |
| `P` | ring front | dropped |
| `"+p` / `"+P` | system clipboard | ring / dropped |
| `"p` / `"P` | the entry picked | ring / dropped |
| `"_p` / `"_P` | nothing — a no-op | untouched |

The removed text goes to the **ring**, never to the sink: `"+p` reads the
clipboard, but what it displaced is ordinary editing history and does not
belong on the system clipboard. The sink names where the paste comes *from*.

### Kinds

The selection has a kind and so does the entry, and the interesting cases are
the ones where they disagree. Every row matches vim and is pinned by
`scripts/vim_differential.py` — through a pty, because `vim -es` drops a visual
mode paste on the floor and would happily agree with anything:

| Selection | Entry | Result |
|---|---|---|
| charwise | charwise | the range becomes the text |
| charwise | linewise | the line splits at the selection; the lines land between the halves |
| linewise | charwise | the lines become one new line holding the text |
| linewise | linewise | the lines become the entry's lines |
| charwise / linewise | blockwise | the range goes, and the block is inserted where it was |
| blockwise | charwise | every row's span becomes the text |
| blockwise | linewise | the block goes, and the lines are opened below its last row |
| blockwise | blockwise | the block goes, and the entry's block replaces it at the corner |

**Cursor lands** exactly where a plain paste of that entry would leave it: on
the last char of charwise text, on the first non-blank of the first pasted line
for linewise, on the top-left corner for blockwise.

**Counts repeat the content**, as they do for a plain paste: `viw3p` replaces
the word with three copies, in one edit and one undo step.

**A paste with nothing to paste changes nothing.** An empty ring says `nothing
to paste` and leaves the selection alone, and `"_p` is the no-op it already is
in normal mode. Vim deletes in both cases, on the reasoning that `v_p` *is* a
delete followed by a put. That reasoning is fine right up until the register is
empty, at which point "paste" has silently become "delete" — the one outcome
nobody typed `p` for. This is a deliberate divergence, and `"_d` is still there
for anyone who meant the delete.

**Visual mode ends**, leaving normal mode with the cursor where the paste put
it. Leaving the replaced text selected would be defensible, but no operator in
visual mode does that here and `p` is not the place to start.

**`.` repeats it** over the same extent from the cursor, which is how every
other visual operator repeats. Vim does nothing at all here; matching bi's own
rule for `d` and `c` in visual is worth more than matching that.

The repeat re-reads the register rather than remembering the text, so it is
`P` that overwrites word after word with the same entry — `p` put what it
displaced on the front of the ring, and repeating a swap swaps again. This is
not a special case for `.` to correct: `.` replays the command, and reading the
ring's front is what the command does.

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
Action::PasteSelection { capture: bool, count: usize, sink: Sink }
Action::Operate { op, motion, count, sink: Sink }
```

`PasteSelection` is a separate action rather than a flag on `Paste` because
what it does to the buffer has nothing in common with it: the range comes from
the selection, the edit replaces rather than inserts, and it ends visual mode.
`capture` is the `p`/`P` difference. It sits beside `OperateSelection` for the
same reason that one exists — visual mode's commands take a selection, not a
motion, and the split is what keeps `Buffer` out of the selection business.

`Buffer` grows the mirror of `paste`:

```rust
pub fn paste_over(
    &mut self,
    start: usize,
    end: usize,
    linewise: bool,     // the *selection's* kind
    entry: &Entry,
    count: usize,
) -> (Entry, Cursor)   // what was removed, and where the cursor landed
```

It returns the removed text instead of capturing it, exactly as `operate_range`
does, so `P` dropping it stays a caller-side policy. A blockwise *selection* is
not a char range and does not go through here; it is spans, and `Editor` walks
them the way `operate_block` already does.

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

Paste over a selection, in `editor.rs`, one per row of the kinds table plus:

- `p` leaves what it replaced on the front of the ring, `P` does not
- the paste reads the register the selection is about to overwrite, not the
  text it just removed
- `viw3p` pastes three copies in one undo step
- an empty ring reports and leaves the selection alone
- `"_p` over a selection changes nothing
- visual mode ends, and the cursor lands where a plain paste would leave it

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
