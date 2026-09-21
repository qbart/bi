# Zoom

`images.md` drew a picture at native size and cropped what did not fit:
"fit-to-window is a zoom feature; zoom is deliberately out until someone
misses it." A tile sheet at 16 pixels a tile is missed at once — a tile is a
thumbnail-sized smudge on a high-DPI screen, and the tilemap's dashed cursor
is the only way to tell where one ends.

## Status

**Built.**

## The command

```
:zoom          says what it is: zoom=2x
:zoom 0        back to 1x
:zoom +        double         :zoom -    halve
:zoom 5        five times     :zoom 0.1  a tenth
```

A number, not `5x` — `5` is enough. Clamped to 0.05 through 32: below that
a sheet is a dot, above it a pixel is a pane. On a window with no image the
answer is `no image here`. `:zoom` rather than `:set zoom` for the same
reason `:set editor` is not an option: zoom is a fact about one picture, and
the option layers have no picture in them.

**Zoom lives on the image**, like its scroll and its tilemap. A bare `:vs`
clones it, `Ctrl-^` brings it back, `:bd` throws it away with the rest.

## The core stays in image pixels

`Img` gains one field, `zoom: f32`, and nothing else moves: scroll, the tile
cursor, `cursor_rect`, follow-the-cursor — every number the core holds is in
the image's own pixels, as before. What changes is what the frontend
*reports*: the viewport it hands `set_viewport` is the pane's pixels divided
by the zoom, and the step is one cell height divided by the zoom. At 2x a
pane shows half as many image pixels; at 0.1x, ten times as many; the clamp
and the follow are the same code. A viewport that rounds to zero is reported
as one pixel, so a 32x zoom in a narrow pane still scrolls.

The status row says `1920×1080 2x` where it said `1920×1080` — after the
size, only when the zoom is not 1x.

## The terminal frontend: two ways to scale

The renderer works in **display pixels**: the image's size times the zoom,
rounded. The crop the core chose (in image pixels) becomes a display-pixel
rectangle, and that is what is placed. There are two ways to get the
terminal to show it, and the budget picks.

**Crisp, within the budget.** When the scaled image is at most 64 MiB of
RGBA — 4096×4096 display pixels — the frontend scales the pixels itself,
nearest-neighbour, up or down, and uploads the *scaled* copy under the
image's id. Nearest-neighbour because the pictures this is for are pixel
art, and a linear filter turns a 16-pixel tile into a blur; a photograph at
0.1x drops rows, which is what a thumbnail does. The upload stamp becomes
the image's generation *and* its zoom, so `:zoom +` re-uploads and `l` does
not. From there the placement is exactly what `images.md` built — crop and
cells on the uploaded pixels, native size, no scaling asked of the terminal.

**Terminal-scaled, beyond it.** A 2048×2048 sheet at 4x is 256 MiB of pixels
and does not go up. The original stays uploaded and the placement asks the
terminal to fit the crop into a cell rectangle (`c=`/`r=`) — the terminal
scales, and every terminal that speaks the protocol scales with a linear
filter, so this path is soft at the edges. It is also a whole number of
cells, so the image can be stretched by up to one cell; a placement that
would be exact needs a crop whose scaled size is a multiple of the cell,
which the budget case is not worth. Honest and rare: `Place::fit` says when
this path was taken.

**The tilemap frame** is rebuilt at tile size times zoom, so its dashes stay
one display pixel wide at every zoom, and it always takes the crisp path —
it is a rectangle. Its placement math is the same display-pixel math as the
picture's.

## Tests

- `set_zoom` clamps to 0.05..32; `+` doubles and `-` halves; `0` resets.
- `:zoom` reports; `:zoom 5`, `:zoom 0.1`, `:zoom +`, `:zoom -`, `:zoom 0`
  each land where they say; `:zoom x` and `:zoom -3` are refused; `:zoom` on
  a text buffer says `no image here`.
- The viewport reported at 2x is half the pane in image pixels, at 0.5x
  twice; the step follows.
- Nearest-neighbour scaling of a 2×2 image at 2x is the expected 4×4, and
  at 0.5x the expected 1×1.
- At 2x the picture's `Place` crops the scaled pixels and covers twice the
  cells; the frame's is twice the tile.
- Over the budget the `Place` carries `fit` and crops the
  original.
- The status row says `2x` at 2x and nothing at 1x.
