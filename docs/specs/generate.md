# Generators: `:uuid4`, `:date`, `:password` and kin

An id in a fixture, a key in a config, today's date in a changelog, a nil
value in a test: the text is known and typing it is thirty-six characters of
nothing. These commands type it.

```
:uuid4              a random uuid
:uuid7              a time-ordered one — sortable, the front is the clock
:uuidzero           00000000-0000-0000-0000-000000000000
:ulid               time-ordered, 26 characters of Crockford base32
:nanoid             21 characters of A-Z a-z 0-9 _ -
:epoch              seconds since 1970
:date [fmt]         today, local time — 2026-09-10, or strftime `fmt`
:time [fmt]         now, local time — 14:03:22, or strftime `fmt`
:lorem [n]          n paragraphs of the classic filler, one unless told
:password [len]     len characters of letters, digits and symbols; 24 unless told
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
times, which is what nil means. `:lorem` puts its paragraphs after the cursor
like everything else — on an empty line, which is where filler goes.

A line range — `:2,5uuid4` — is refused: `uuid4 takes a selection or the
cursor, not a range`. Replacing four lines with one id is not a thing anyone
means, and the command that would do it is a selection away.

## Afterwards

- **One undo step.**
- **The cursor on the last character of each id**, collapsed, as after `p`.
  The selection that named a piece has been consumed.
- Lowercase hex, hyphenated, thirty-six characters. That is the canonical
  spelling and the only one; uppercase is `:case upper` away.

## Each one

- **`:date`** and **`:time`** take an optional strftime format, `:date
  %d.%m.%Y`, and read the local clock. A format that is not one is
  `bad format `%`: …` with the reason, and nothing is inserted. The defaults
  are ISO, which sorts and which every parser reads.
- **`:epoch`** is UTC seconds, no fraction.
- **`:lorem [n]`** is deterministic — the same n gives the same text —
  because filler that changes under you is filler you cannot diff. Each
  paragraph starts one sentence further in, so two in a row read
  differently. `n` must be a number above zero: `lorem how many? `:lorem 3``.
- **`:password [len]`** draws uniformly from letters, digits and
  `!@#$%^&*-_=+` — symbols no shell, URL or JSON string chokes on. The
  default is 24. `len` must be a number above zero.
- **`:ulid`** and **`:nanoid`** are the two ids the web has settled on
  beside uuid, in their canonical spellings.
- The bare generators take no argument and say so: `uuid4 takes no
  argument`.

## Where it lives

`src/generate.rs` holds the three as one enum — nothing in, text out:

```rust
pub enum Generator {
    Uuid4, Uuid7, UuidZero, Ulid, NanoId, Epoch,
    Date(Option<String>), Time(Option<String>), Lorem(usize), Password(usize),
}

impl Generator {
    pub fn parse(name: &str, arg: &str) -> Option<Result<Self, String>>;
    pub fn generate(&self) -> Result<String, String>;
}
pub const NAMES: &[&str];
```

The enum is the point: every "put this text here" command is another arm
and nothing new in the editor, which asks `parse` whether a name is one of
these and never lists them itself. The doing is `View::generate`, which
finds the pieces — an empty one after each cursor, or each selection —
generates one value per piece, and writes through `Region::replace`, the
same write `:base64e` uses.

The ids come from the `uuid` and `ulid` crates, the clock from `jiff`, the
random draws from `rand`. Randomness and time zones are things to get right
once, and those crates are where they have been gotten right.

## Tests

In `generate.rs`:

- `uuid4` is thirty-six lowercase hex-and-hyphen characters with the version
  nibble `4`; two of them differ.
- `uuid7` has version nibble `7`, and two made in order sort in order.
- `uuidzero` is the nil uuid; `ulid` and `nanoid` have their lengths and
  alphabets.
- `epoch`, `date` and `time` read the clock; a format is honoured and a bad
  one refused.
- `lorem` counts paragraphs, differs between them, and wants a number.
- `password` has the length asked for, from its alphabet.

In `editor.rs`:

- `:uuid4` on a line puts an id after the cursor, cursor on its last char.
- `:'v uuidzero` replaces the selection with the nil id.
- two cursors get two different `:uuid4`s and the same `:uuidzero`.
- `:2,3uuid4` is refused and changes nothing.
- one undo step.
- `:lorem 2`, `:password 8` and `:date %Y` land after the cursor with the
  shapes they promise; a bad count and a stray argument are refused.
