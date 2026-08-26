# Encoding, BOM, line endings

bi was UTF-8 with `\n` line endings, end to end: `Rope::from_reader` refused
any other file at the door. For a repository born this decade that is the
right answer, but an editor whose answer to a 1998 `latin1` header or a
Windows-team `.csv` is an error message is an editor a vim user cannot move
to. Vim opens those files without being asked; so must bi.

## The model

**The rope is always UTF-8 with `\n`.** Encoding, BOM and line endings are
facts about how a file is *stored*, not about what it *says*, so they live at
exactly one boundary: decode on open, encode on save. Nothing above
`Buffer::open`/`save` — motions, search, LSP, tree-sitter, every frontend —
ever sees a non-UTF-8 byte or a `\r\n`. That is also what keeps the core
embeddable: frontends deal in UTF-8, full stop.

`Buffer` carries the three facts the way it carries `path`:

```
encoding    &'static encoding_rs::Encoding   what the bytes on disk are
bom         bool                             whether the file leads with a BOM
fileformat  Unix | Dos                       what ends a line on disk
```

They are **buffer state, not options**. An option resolves from five layers
and a `:set` lands in the session-wide layer (`options.md`); "this file is
cp1250" is not an opinion any layer holds about every buffer, it is something
*detected about one file*, overridable for that file. So `:set fileencoding`
is special-cased to the buffer in view — which is also vim's semantics, where
`fileencoding` and `fileformat` are buffer-local.

Transcoding is `encoding_rs` — the WHATWG set: every `windows-125x`,
`latin1`, `koi8`, `shift_jis`, `euc-jp`, `gbk`, `big5`, `utf-16le/be`, and a
label table that already accepts vim spellings (`latin1` and `cp1250` are
labels for `windows-1252` and `windows-1250`). One deliberate absence:
`encoding_rs` will not *encode* to UTF-16 (the Encoding Standard forbids it
for the web), so bi writes UTF-16 by hand — it is `char` code units and a
byte order, twenty lines, not a dependency.

## Open

Read the bytes once, then in order:

1. **BOM** (UTF-8, UTF-16LE, UTF-16BE) wins outright and sets `bom = true`.
2. Otherwise walk the **detection list** — `fileencodings` in config, default
   `["utf-8", "latin1"]` — and the first encoding that decodes the whole file
   *without error* wins.

The default list is why there is nothing to configure: `latin1` maps every
byte to a char, so the walk cannot fall off the end and opening cannot fail.
A real UTF-8 file never mis-detects as latin1 (UTF-8 is tried first); a
latin1 file essentially never decodes as valid UTF-8. The list is
deterministic and explainable — no statistical guessing, exactly vim's
`fileencodings` mechanism. Anyone who actually lives in `cp1250` writes
`fileencodings = ["utf-8", "cp1250"]` in `config.toml` once and is done;
the key sits at the top level beside `[alternate]`, not in `[options]`,
because it is input to detection at open, not a fact any one buffer holds.

**Line endings:** after decode, a file every one of whose `\n`s is preceded
by `\r` is `dos`; the `\r`s are stripped from the rope. A *mixed* file stays
`unix` with its stray `\r`s visible in the text, vim-style — classifying it
as `dos` would silently rewrite lines the user never touched on the next
save. A file with no newlines at all is `unix`.

A new buffer — no path, or a path that does not exist yet — is `utf-8`,
`unix`, no BOM, before the project gets its say (below).

## Save

The reverse, in one streaming pass: re-insert `\r` before each `\n` if
`dos`, prepend the BOM if `bom` (UTF-16 always writes one — a bare UTF-16
file is unreadable by convention), encode from UTF-8 to `encoding`.

**An unencodable character fails the write.** You typed `€` into a `latin1`
file: the save stops, nothing is written, and the message names the character
and its `line:col`, so the fix is one `:<line>` away. This is vim's E513 line
of defence and the one non-negotiable rule here — the editor must never
silently write replacement bytes into someone's file. The way out is the
character's removal or `:set fileencoding utf-8`.

A save clears the storage-dirty flag (below) along with marking history
saved.

## Controls

Deliberately few, deliberately vim-shaped:

- `:set fileencoding cp1250` — this buffer's encoding; the next `:w`
  converts the file. `:set fileformat dos|unix` and `:set bom true|false`
  the same. Any of the three marks the buffer modified — the text no longer
  matches the disk *as stored* — via a `storage_dirty` flag on the buffer
  that ORs into `is_modified` and clears on save, since history alone cannot
  see a change that touches no text.
- `:set fileencoding` (no value) reports the buffer's current encoding, the
  same way bare `:set syntax` reports what is in force.
- `:e ++enc=cp1250`, `:e ++ff=dos` — reopen with detection overridden, for
  the file the list guessed wrong. With a path it is `:e ++enc=… <path>`;
  bare, it is the revert form and re-reads the current file. The `++`
  spelling is vim's, kept verbatim: muscle memory is the point of this whole
  spec. Bare `:e` re-runs detection, also as in vim.
- `fileencodings = [...]` — the detection list, `config.toml` top level.
- The status row shows a badge only when a buffer is not plain
  `utf-8`/`unix`: `header.h [+] [latin1] [crlf]`. The default state is
  silence — a clean UTF-8 file's status row does not change by one cell.

## The project's say

`.editorconfig`'s `charset` and `end_of_line` stop being ignored
(`editorconfig.md`). They are not options, so they do not travel through the
`OptionPatch` layer; they apply at the same boundary the facts live at —
open:

- `charset` moves its encoding to the *front of the detection list* for that
  file. Preference, not force: a file whose bytes refuse the project's
  charset still opens through the rest of the list, because refusing to open
  a file over a `.editorconfig` line would be worse than the guess being
  wrong. `charset = utf-16le/be` also sets the BOM expectation editorconfig
  defines for those values.
- `end_of_line` sets `fileformat` outright — that is the property's
  documented meaning, a file not yet conforming converts on its next save.
- Both set the initial state of a **new** file, so a file born in a `crlf`
  project is born `dos`.

Order stays temporal and simple: detection and editorconfig at open, your
`:set` after it, on the one buffer you said it to.

## What this deliberately is not

- No statistical charset detection (`chardet`): wrong often enough to be a
  support burden, unexplainable when it is. The list is the mechanism.
- No `encoding` option for the rope's internal coding — neovim already made
  this call: the inside of the editor is UTF-8 and is not a setting.
- No transcoding anywhere else. `:grep`/find-in-files and git blobs read
  bytes off disk and match them as UTF-8; a latin1 file's `ü` will not match
  a search for `ü` outside an open buffer, and a non-UTF-8 file's git signs
  may mark more than changed. Known, accepted: the boundary stays at buffer
  I/O until someone actually hits this.

## Testing

Round-trip fixtures per encoding — open, save untouched, byte-identical
file, BOM and `\r\n` included. Detection-order cases: UTF-8 wins over
latin1, BOM over both, `charset` moves the front. The unencodable save
fails, names `line:col`, leaves the file untouched. Mixed endings stay
`unix` and survive a round trip. `:e ++enc=` re-decodes the same bytes;
`:set fileencoding` marks modified and converts on write; the badges appear
and disappear.
