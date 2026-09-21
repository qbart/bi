# Image operations

`:tool tileset` moves tiles around a sheet. This is the other half of
touching a picture: the whole of it at once — turned to grey, resized,
cropped, blurred, written as a different format. One-shot operations from
the ex line, each one undo step, on the image in the focused window.

## Status

**Built.**

## The commands

```
:tool image grayscale
:tool image invert
:tool image flip x                  or y
:tool image blur 2.5                sigma, in pixels
:tool image resize 64,64            pixels — the tileset's resize is in tiles
:tool image resize 64,64 --alg lanczos      nearest (the default) | bilinear | lanczos
:tool image crop 0,0,32,32          x, y, width, height
:tool image conv jpg                png | jpg | webp | bmp
:tool image conv png --rgb          --rgb drops alpha on write, --rgba keeps it
:tool image conv jpg --quality 80   1..100, jpg only; 90 unless said
```

Pairs and quads take commas, as everywhere on the ex line. Flags are
`--name value` or `--name`, after the positional arguments, in any order.
An unknown operation says `image what? (grayscale, invert, flip, blur,
resize, crop, conv)`; on a window with no image every one says `no image
here`.

**`resize` defaults to nearest.** The pictures this editor is for are
sheets and sprites, where a filter that blends is a filter that ruins;
`--alg lanczos` is there for a photograph. `blur` is a Gaussian by sigma.
`crop` is refused rather than clipped when the rectangle runs past the
image: `0,0,64,64 runs past 50×40`.

Every pixel operation is **one undo step** on the image's own history —
the same stack the tileset's `dd` and `image resize` use, so `u` after a
`grayscale` brings the colours back, in the tileset or out of it. A
whole-sheet edit remembers both versions of the pixels, before and after,
so undo and redo are copies rather than recomputation; an operation is not
guaranteed reversible, and `blur` certainly is not.

## `conv` and what `:w` writes

`conv` changes nothing in memory. It sets the **format** the image is
written in — the encoder, whether alpha goes with it, the JPEG quality —
and re-points the path's extension to match, so `:w` on `atlas.png` after
`conv jpg` writes `atlas.jpg`, and says so in the status row. `:w` keeps
its one meaning: the image in the window goes to that image's own file.
Writing PNG bytes into a `.jpg` name is what this rule exists to prevent.

The format is set from the extension when a file opens, and again by
`:w other.webp`, the way `:w other.rs` re-types a buffer. PNG, JPEG, WebP
and BMP can be written; anything else is refused, `cannot write gif (png,
jpg, webp, bmp)`, rather than written as PNG under the wrong name. JPEG
has no alpha, so `--rgb` is implied for it; WebP is written lossless, the
only kind the encoder knows, and `--quality` is ignored there with a note.

## `:e` reloads

`:e` on a picture is what it is on a buffer: the file as it is on disk
now, refused on a dirty picture without the `!`. The reload is one undo
step, so what `:e!` discarded is one `u` away, and the crop, zoom, grid
settings and format all stay. A file that no longer decodes changes
nothing and says why.

## In the core

`src/imgops.rs` holds the operations as functions from pixels to pixels —
`grayscale`, `invert`, `flip`, `blur`, `resize`, `crop` — over the `image`
crate's `imageops`, and the `Format` the writer reads. `Img` gains
`format: Format` and one method per operation, each of which replaces the
sheet through the tileset's `replace`, which is where the undo step comes
from. The frontend sees a new generation and re-uploads, as after any
edit.

## Tests

- `grayscale` makes every pixel's channels equal and is one `u` away from
  the colours; `invert` twice is the image it was.
- `flip x` mirrors columns, `flip y` rows.
- `resize 4,4` on a 2×2 with nearest is four blocks; `--alg lanczos` is
  accepted and `--alg cubic` refused.
- `crop 1,1,2,2` is the rectangle; a crop past the edge is refused and
  changes nothing.
- `conv jpg` re-points `photo.png` to `photo.jpg` and `:w` writes a JPEG
  there; `conv png --rgb` writes a PNG without alpha; `:w x.gif` is refused
  with the list; `--quality 200` is refused.
- Every operation is refused on a text window with `no image here`.
- `u` after `resize` restores the dimensions and every pixel.
- `:e` on a dirty picture is refused; `:e!` loads what is on disk, keeps
  the zoom and grid, is one `u` away; a clean picture reloads on `:e`.
