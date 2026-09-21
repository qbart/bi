# Tileset

`images.md` made `:e atlas.png` show the atlas. It still could not touch a
pixel of it, and the one thing a tile sheet wants touched is a *tile*: this
one moved there, that one blanked, the two swapped. A pixel editor is a
different program; a tile mover is a cursor over a grid, three keys and a
`:w`, and bi already has the cursor keys, the register idea and the picture.

## Status

**Built.**

**Tileset, not tilemap.** The picture is a *tileset* — the sheet the tiles
come from. A *tilemap* is a grid of references into one, the thing a TMX
file holds, and that name is kept for the spec that reads those.

## What it is

A mode of one picture, not of the session. `:set editor tileset` on an image
window puts a grid over it; the cursor is one cell of that grid, drawn as a
dashed rectangle the size of one tile, starting at pixel 0,0. `Esc` takes the
grid away. Nothing else leaves the mode — `:` still opens the ex line, which
is how `:w` and `:q` still work, and `Ctrl-W` still moves between windows,
because the mode lives on the image and focus moving off it changes nothing.

```
:set editor tileset          the grid goes on
:set editor image            and off again — what Esc does
:set editor                  says which

:set tileset size 16x16      one tile, in pixels; the default
:set tileset size 16         square, same thing
:set tileset size            says what it is
:set tileset kind tile       the only kind that is built
:set tileset kind hex        parses, and is refused: "hex is not built yet"
```

These are spelled `:set` and are **not options**. Options resolve per
buffer, and an image is a `Content` on a window, not a buffer — there is no
`[options]` line, no `[filetype.png]`, no layer stack. `fileencoding` set
the precedent: buffer-local facts handled in `set_option` before the option
lookup. `editor` and `tileset` are handled the same way, on the focused
window's image, and on any other kind of window say `no image here`.

Size and kind stay on the image after `Esc`, so leaving and coming back
finds the grid where it was. They can be set before the mode is entered or
after; a size change re-clamps the cursor.

**Whole tiles only.** A 100×100 sheet with 16-pixel tiles is a 6×6 grid; the
four-pixel remainder is not a tile, the cursor never reaches it, and `:w`
writes it back untouched. A sheet smaller than one tile has no grid, and
entering the mode says so.

## The model

```rust
pub struct Tileset {
    size: (u32, u32),          // one tile, in pixels
    kind: Kind,                // Tile; Hex is parsed and refused
    cursor: (u32, u32),        // column, row — in tiles
    undo: Vec<Edit>,
    redo: Vec<Edit>,
}

pub struct Tile { pub width: u32, pub height: u32, pub rgba: Vec<u8> }
```

`Img` gains three fields beside its pixels: `tileset: Option<Tileset>` —
`Some` exactly while the mode is on — plus `dirty: bool` and
`generation: u64`. The tileset's size and kind outlive the `Option`: they
are kept on the image (`tile_size`, `tile_kind`) and the `Tileset` is built
from them on entry. That is what "stays after Esc" is made of.

**One register slot.** `Session::tile: Option<Tile>` — shared by every
image in the session, so a tile yanked from one sheet pastes into another.
One slot rather than a ring: the text ring's picker is built for prose, and
a ring of pictures wants a picker that draws them. It can come when someone
misses it. Separate from the text ring on purpose: text and pixels do not
paste into each other, so one ring for both would be two rings wearing one
coat.

**Undo is per image.** Each `dd` and `p` records the tile it overwrote —
position and the pixels that were there — on the image's undo stack, and
`u` puts them back, moving the edit to the redo stack for `Ctrl-R`. A fresh
edit clears redo, as text undo does. `dd` without undo is a scary key.

**Dirty and generation.** Every edit sets `dirty` and bumps `generation`.
`dirty` is what `:q` reads; `generation` is what a frontend that uploaded
the pixels once reads, to know the upload is stale.

## The keys

No `KeyMode::Tileset`, no `[keys.tileset]`: the window still dispatches
through the normal keymap, and `Editor::run_image_action` — which already
reads a handful of normal-mode actions as pixels — reads them as tiles when
the tileset is on:

```
h j k l        one tile; counts multiply
0  ^           first column          $  g_    last column
gg             first row             G       last row;  5G  row 5
Ctrl-D/U       half a viewport of rows, down / up
dd             yank the tile into the slot, then clear it to transparent
yy             yank the tile into the slot
p  P           paste the slot over the tile under the cursor
u  Ctrl-R      undo, redo
rh  rl         turn the tile a quarter left, right
rj             turn it half way round
rk             mirror it top to bottom      rx  ry    mirror left-right, top-bottom
Esc            leave the mode
```

`dd` cuts, so `dd` here and `p` there moves a tile — vim's semantics, and
the operation a sheet most wants. Clear means transparent, `(0,0,0,0)`: the
sheet is RGBA in memory whatever it was on disk, and PNG keeps the alpha.

`p` with an empty slot says `nothing to paste`. `p` with a slot whose size
is not the grid's says so — `tile is 16×16, grid is 32×32` — and does
nothing; a clipped paste is a guess about which corner you meant. `p` and
`P` are the same key: a tile has no before and after.

**`r` turns the tile in place.** In a text buffer `r` waits for the
character to put under the cursor, so `rh` already arrives as one action
carrying `h`; the tileset reads the character as a direction. `rh` and `rl`
are quarter turns, `rj` a half turn, `rk` the half turn followed by a
left-right mirror — which is a top-to-bottom mirror, and `ry` spells the
same thing so the mirrors read as a pair with `rx`. Any other character
says `rotate what? (r + h j k l x y)`. A quarter turn of a 16×8 tile is an
8×16 tile that does not fit its cell, so `rh` and `rl` are refused on a
tile that is not square — `16×8 does not turn` — while the half turn and
the mirrors work at any size. Every turn is one undo step, like a paste.

**The crop follows the cursor.** After every move the scroll shifts by the
least that puts the cursor's tile fully inside the viewport — so `G` on a
tall sheet scrolls to the bottom row, and `gg` back — and stays put when it
already is. A viewport smaller than one tile shows the tile's top-left.

Everything the plain image swallows, the tileset swallows too: `i`, `v`,
`/`, `s` still do nothing. `Esc` in a plain image window is still nothing;
in the tileset it is the way out.

## Saving and quitting

`:w` on an image encodes its pixels as PNG to its path; `:w other.png`
writes there and re-points the image at the new name, the way `:w other.rs`
re-points a buffer. Any other extension is refused — `only png` — rather
than silently writing PNG bytes under a `.jpg` name. A successful write
clears `dirty` and says `"atlas.png" written`. `:w` works whether or not the
tileset is on: the edits are the image's, and leaving the mode does not
throw them away.

`:q`, `:bd` and `:qa` refuse a dirty image the way they refuse a dirty
buffer — `unsaved changes (use `:q!` to discard)` — and `!` forces. `:q`
refuses even with other windows open, because closing an image window
discards the image: there is no buffer list holding it. `:qa` names the
image the way it names a buffer. `:wq` writes then quits.

## The frontend

The core says where the cursor is in pixels — `Tileset::cursor_rect()`,
x, y, width, height in image coordinates — and a frontend draws that
however it draws. The terminal frontend draws it as a second kitty image.

**A second placement, not a re-upload.** One small image — the dashed frame,
tile-sized, transparent inside — is uploaded once per tile size under an id
the frontend reserves (`u32::MAX` — the protocol's ids are 32-bit, and the
core's counter never climbs that far),
and rebuilt when the size changes. Each frame it is placed at `z=0` over
the atlas's `z=-1`, at the cell the tile's top-left lands in with the pixel
remainder in the placement's `X`/`Y` offset, cropped where the tile runs
past the pane. Moving the cursor is one placement escape and no upload;
compositing the frame into the pixels would re-send the whole sheet per
`h`, which a 2048×2048 atlas turns into visible lag. The frame's dashes
alternate black and white in three-pixel runs, so they read on any tile,
light or dark.

The atlas placement and the frame placement share the window's placement
id — placement ids are per image id in the protocol, so they do not
collide, and the frame's placement goes away with the window's the same
way.

**Edits re-upload.** `Graphics::sent` remembers the generation each id was
sent at; a placement whose image's generation moved is transmitted again
under the same id, and its placements re-emitted, because the protocol
replaces the pixels and forgets the placements with them. The picker rule
is unchanged: a frame under the overlay is dropped for those frames.

**Without graphics** the mode still works, blind: the status row is the
whole feedback. That is honest — the edits are the same edits — and it is
what a test sees.

## The status row

The left half of an image's row says `1920×1080`; with the tileset on it
says `1920×1080  16×16 tile 3,2 of 6×6`, and a `+` after the name when the
image is dirty, as a buffer's row does. The right half — the mode segment
the plain image omits because it has no modes — says `TILESET`, because
this one does.

## Tests

- `:set editor tileset` on a text buffer says `no image here`; on an image
  the tileset is on with the cursor at 0,0; `:set editor image` and `Esc`
  turn it off; `:set editor` reports.
- `:set tileset size 16x16` and `16` are the same; `size` reports; a bad
  size says so; `kind hex` is refused; `kind tile` is accepted.
- The grid is whole tiles: 100×100 at 16 is 6×6; the cursor clamps to it;
  a size change re-clamps.
- `hjkl` move by tiles and counts multiply; `0`/`$`/`gg`/`G` hit the grid's
  edges; `5G` goes to row 5.
- The crop follows the cursor and stays put when it already contains it.
- `yy` puts the tile's pixels in the slot; `dd` puts them there and leaves
  transparent behind; `p` writes the slot over the cursor's tile; the slot
  survives `Esc` and a second image.
- `p` with an empty slot and `p` with a mismatched size are refused with a
  message and change nothing.
- `u` restores what `dd` cleared; `Ctrl-R` clears it again; a new edit
  drops redo.
- `rl` then `rh` is the tile it was; four `rl` are too; `rj` is two `rl`;
  `rk` is `rj` then `rx`, and equals `ry`; each is one undo step.
- `rl` on a 16×8 tile is refused with `16×8 does not turn` and changes
  nothing; `rj` on it works.
- `rq` says `rotate what? (r + h j k l x y)`.
- An edit sets `dirty` and bumps `generation`; `:w` round-trips the pixels
  through a PNG on disk and clears `dirty`; `:w a.jpg` is refused.
- `:q` on a dirty image refuses, `:q!` closes; `:bd` the same; `:qa` names
  the image.
- The render emits a frame `Place` at `z=0` over the tile's cell with the
  pixel remainder in its offset, and none when the tileset is off.
- A dirty image's status row carries `+`, the tile coordinates, and a
  `TILESET` mode segment.
