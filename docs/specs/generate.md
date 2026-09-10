# `:uuid4`, `:uuid7`, `:uuidzero`

An id in a fixture, a key in a config, a nil value in a test: the text is
known and typing it is thirty-six characters of nothing. These commands type
it.

```
:uuid4        a random uuid
:uuid7        a time-ordered one — sortable, the front is the clock
:uuidzero     00000000-0000-0000-0000-000000000000
```

## Status

**Built.**

## Where it goes

**After the cursor when nothing is selected, in place of the selection when
something is.** Pressing `:` with a selection up prefills `'v`, so the second
case reads `:'v uuid4` and says so before you press Enter.

*After* the cursor rather than before, for the reason `p` is: the usual
shape of this is typing up to where the id goes, pressing `Esc`, and asking
for one. `Esc` steps the cursor back onto the last character typed, and
"after it" is exactly where insert mode was. On an empty line the two are the
same place.

**Every cursor gets its own.** Three cursors and `:uuid4` is three different
ids, which is the multi-cursor promise; `:uuidzero` is the same nil three
times, which is what nil means.

A line range — `:2,5uuid4` — is refused: `uuid4 takes a selection or the
cursor, not a range`. Replacing four lines with one id is not a thing anyone
means, and the command that would do it is a selection away.

## Afterwards

- **One undo step.**
- **The cursor on the last character of each id**, collapsed, as after `p`.
  The selection that named a piece has been consumed.
- Lowercase hex, hyphenated, thirty-six characters. That is the canonical
  spelling and the only one; uppercase is `:case upper` away.

## Where it lives

`src/generate.rs` holds the three as one enum — nothing in, text out:

```rust
pub enum Generator { Uuid4, Uuid7, UuidZero }

impl Generator {
    pub fn generate(self) -> String;
}
```

The enum is the point: a timestamp, a lorem paragraph, a random number —
every "put this text here" command is another arm and nothing new in the
editor. The doing is `View::generate`, which finds the pieces — an empty one
after each cursor, or each selection — and writes through `Region::replace`,
the same write `:base64e` uses.

The ids come from the `uuid` crate, `v4` and `v7` features. Randomness is a
thing to get right once, and that crate is where it has been gotten right.

## Tests

In `generate.rs`:

- `uuid4` is thirty-six lowercase hex-and-hyphen characters with the version
  nibble `4`; two of them differ.
- `uuid7` has version nibble `7`, and two made in order sort in order.
- `uuidzero` is the nil uuid.

In `editor.rs`:

- `:uuid4` on a line puts an id after the cursor, cursor on its last char.
- `:'v uuidzero` replaces the selection with the nil id.
- two cursors get two different `:uuid4`s and the same `:uuidzero`.
- `:2,3uuid4` is refused and changes nothing.
- one undo step.
