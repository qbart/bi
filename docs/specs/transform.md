# Transforms: `:base64e`, `:hexe`, `:urle`, `:jsonfmt`, `:md5` and kin

A token in a config file, a blob in a JSON fixture, a header in a request
log: encoded text turns up in text you are editing, and reading it means
leaving the editor to `echo | base64 -d` and coming back. These commands are
that round trip, in place.

```
:base64e  :base64d     base64
:hexe     :hexd        hex
:urle     :urld        percent-encoding
:jsonfmt  :jsonmin     JSON, pretty-printed or minified
:md5      :sha256      the digest, in place of the text
```

Every one is *text in, text out*, over the same scope, with the same
afterwards. Below, "encode" stands for whichever of them you ran.

## Status

**Built.**

## What it acts on

**Whatever the scope on the `:` line names**, and with none, **the whole
file** — see [ranges.md](ranges.md). Pressing `:` with a selection up
prefills `'v`, so the common case is "the selection" and it is visible before
you press Enter:

| | |
|---|---|
| `:'v base64e` | exactly what is selected — a charwise selection's characters, each row of a rectangle, every cursor's own |
| `:'<,'>base64e` | the rows the selection touches, whole |
| `:2,5base64e` | lines 2 to 5, whole |
| `:base64e` | the whole file |

**Each contiguous piece is encoded on its own.** A charwise selection across
three lines is one piece — the newlines inside it are part of the text and go
into the encoding — and comes back as one line of base64 where the three
were. A rectangle is one piece per row, because that is what a rectangle is.
Several cursors are several pieces. A line range, or the whole file, is one
piece: the rows joined by their terminators, encoded as one, replacing the
rows as one — and the terminator after the last row stays where it was, so
`:base64e` then `:base64d` is the file you started with.

An empty file, or a selection with nothing in it, says `nothing to encode`
and does nothing.

## Each one

**`:base64e`** — the standard alphabet, `+` and `/`, padded with `=`, on one line — no
wrapping at 76 columns. Wrapping is a transport concern and every decoder
ignores it anyway; the editor's job is the bytes.

**`:base64d`** is **lenient on the way in**, since the text was pasted from
somewhere: whitespace anywhere in it is ignored, so a blob wrapped by
whatever produced it decodes as one; the URL-safe alphabet (`-` and `_`) is
read as well as the standard one; and the `=` padding may be there or not.

It is **strict on the way out**. A character outside both alphabets is an
error — `not base64: \`x\` at 12` — and nothing changes. Bytes that decode
to something other than UTF-8 are an error too, `decoded bytes are not
UTF-8`, and nothing changes: the buffer holds text, and a decoded PNG
sprayed into it as replacement characters is not what anyone asked for.

**`:hexe`** is the UTF-8 bytes as lowercase hex, no separators. **`:hexd`**
reads either case, ignores whitespace, names a stray character with its
offset — `not hex: \`g\` at 1` — and refuses an odd count of digits, since
half a byte is not a byte.

**`:urle`** percent-encodes every byte outside RFC 3986's unreserved set
(`A-Z a-z 0-9 - _ . ~`), a space included, as `%XX` per UTF-8 byte.
**`:urld`** turns `%XX` back and leaves a `+` a `+`: reading it as a space
is the form-encoding dialect, and a query string is not the only place a URL
escape turns up. A `%` without two hex digits after it is an error naming
the offset.

**`:jsonfmt`** pretty-prints, one indent unit per level, **the buffer's
unit** — `tab_width` and `expandtab` as they stand for this file, so a
tab-indented file gets tabs. Keys keep the order they had; a formatter that
sorts them has changed the document. **`:jsonmin`** removes every
insignificant byte. Anything that is not JSON is `not JSON: …` with the
parser's own reason, and nothing changes.

**`:md5`** and **`:sha256`** replace the text with its digest, lowercase
hex. There is no decoding a digest, so the pair is one command.

Every piece is transformed before any is written. Two selected blobs where
the second is broken is an error and an unchanged buffer, not one blob
decoded and a message about the other.

## Afterwards

- **One undo step**, like every `:` command that rewrites text.
- **One cursor per piece, at its start**, collapsed. The selection that named
  the piece has been consumed; what stands in its place is the start of the
  new text, which is where the next thing you do begins.
- The report says what was done — `encoded`, `decoded`, `formatted`,
  `minified`, `hashed` — with a count, `3 pieces decoded`, when there was
  more than one.

## Where it lives

`src/transform.rs` holds the pair as one enum — text in, text out or an
error, and no knowledge of a buffer anywhere in it:

```rust
pub enum Transform {
    Base64Encode, Base64Decode, HexEncode, HexDecode, UrlEncode, UrlDecode,
    JsonFormat { indent: String }, JsonMinify, Md5, Sha256,
}

impl Transform {
    pub fn parse(name: &str) -> Option<Self>;
    pub fn apply(&self, text: &str) -> Result<String, String>;
}
pub const NAMES: &[&str];
```

The enum, rather than a function per command, is the point: every "this
text, spelled differently" command is an arm here and nothing new in the
editor, which asks `parse` whether a name is one of these and never lists
them itself. The doing is `View::transform`, beside `recase`, which resolves
the scope through `View::region`, joins a line range into one piece, fills
in the indent unit for `:jsonfmt`, applies the transform to each piece, and
writes them back through `Region::replace`.

## Tests

In `transform.rs`, no buffer involved:

- every name parses and a stranger does not.
- base64 encoding is standard-alphabet, padded, unwrapped; decoding accepts
  whitespace, both alphabets, and missing padding; a bad character names
  itself and its offset.
- hex is lowercase out and either case in; an odd digit count is refused.
- `:urle` escapes everything but the unreserved set; `+` survives `:urld`.
- `:jsonfmt` uses the unit it is given and keeps key order; `:jsonmin`
  strips; non-JSON is refused with the parser's reason.
- the digests of `hello`.
- every decoder refuses bytes that are not UTF-8; every pair round-trips.

In `editor.rs`:

- `:base64e` with no scope encodes the whole file and keeps the final
  newline; `:base64d` afterwards is the original.
- `:'v base64e` encodes a charwise selection across lines as one piece.
- a rectangle encodes a piece per row; two cursors are two pieces, and
  `:base64d` afterwards puts two cursors at their starts.
- `:2,3base64e` encodes the rows as one piece and leaves the rest alone.
- a broken piece leaves the buffer untouched, and the message names the byte.
- one undo step.
- `:'v hexe` then `:'v hexd` is the selection back; `:md5` replaces the file
  with its digest.
- `:jsonfmt` indents with the buffer's own unit and `:jsonmin` undoes it.
