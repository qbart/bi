# Normal map generator

A height map is a grey picture; a normal map is what a shader wants
instead. Turning one into the other is a small pipeline with knobs, and
the knobs want turning while the result is in view. `:set editor
normalmap` on a height map puts the result beside it and a form of knobs
beside that.

## Status

**Built** — normal and displacement. Ambient occlusion and specular are
the next step and slot into the same form.

## What it looks like

Three windows: the **source** you started from, the **result** split to
its right at the same zoom, and the **form** as a sidebar down the right
edge of the screen, the mirror of where the tree goes. Source and result
sit side by side so a knob's effect is a glance away. Focus lands on the
form; `Esc` in it goes back to the source, `Ctrl-W` moves as usual.

```
Map        ‹ normal ›            what the result shows: normal | displacement
Strength   ━━━━━●━━━━   2.5      slope scale, 0.1..20
Level      ━━━━━━●━━━   1.0      gamma on the height, 0.1..10
Blur       ●━━━━━━━━━   0        gaussian sigma on the height, 0..32 pixels
Filter     ‹ sobel ›             sobel | scharr | prewitt
Height     ‹ luma ›              luma | r | g | b | a
Invert R   [ ]
Invert G   [ ]
Invert H   [ ]                   the height itself
Z range    ‹ -1..1 ›             -1..1 | 0..1
```

`Tab` anywhere in the form flips the map shown. Every field is also an
ex command — `:tool normalmap strength 2.5`, `:tool normalmap filter
scharr` — and reports without a value, so a preset is a line in a script.

## The pipeline

Pure functions in `src/normalmap.rs`, with no editor in them:

1. **Height** from the source: luma, or one channel, as `0..1` floats;
   inverted if asked; blurred by `blur` sigma.
2. **Level** is a gamma on the height, `h^level`, so the same slopes can
   be pushed toward the peaks or the floor.
3. **Gradient** by the chosen 3×3 filter, Sobel, Scharr or Prewitt,
   edges clamped.
4. **Normal** is `normalize(-dx·strength, -dy·strength, 1)`, with R or G
   flipped if asked — the Y-up versus Y-down question every engine
   answers differently — and encoded as `rgb = n·0.5 + 0.5`; with Z range
   `0..1`, blue is `z` itself.
5. **Displacement** is the levelled height as grey, alpha opaque.

The result is recomputed synchronously whenever the form's generation
moves, and its window's image gets a new generation, so the terminal
re-uploads. A 4096-square sheet with a wide blur will feel it; that is
the honest cost of a live picture, and a worker is the fix when it hurts.

## Writing

`:w` is untouched: it writes the image in the window to that image's own
file. The result window's image is named for its output from the start —
`atlas_normal.png` beside `atlas.png`, `atlas_disp.png` for displacement —
so `:w` there writes that map where you would expect it, and `:w
other.png` names another place. `:tool normalmap write` writes the map
shown; `:tool normalmap write all` writes every map the tool makes, each
under its own name, and says how many.

A result is never dirty: it is derived, and `:q` on it asks nothing. The
source is dirty only if you edit *it*.

## Leaving

`:set editor image` on the source closes the result and the form and
forgets the tool. Closing either of those windows by hand does the same;
closing the source leaves them as a plain picture and a plain form, since
what they showed is still on screen and taking it away would be a
surprise. `:set editor normalmap` on a window that already has the tool
focuses its form.

## Tests

- Height from luma of a grey ramp is the ramp; from `r` is the red
  channel; inverted is one minus.
- A flat picture makes a flat normal map, `(128, 128, 255)` everywhere at
  `-1..1` and `(128, 128, 255)` at `0..1` too, since `z` is 1.
- A left-to-right ramp makes normals leaning one way; `invert r` leans
  them the other; strength 0.1 is nearly flat.
- Displacement of a ramp is the ramp, levelled by the gamma.
- `:set editor normalmap` on a text window says `no image here`; on an
  image it opens two windows, result right of the source and the form at
  the screen's right edge, and focuses the form.
- Turning `strength` in the form changes the result's pixels and bumps
  its generation; `Tab` swaps the result for displacement and its name.
- `:tool normalmap write all` writes `<stem>_normal.png` and
  `<stem>_disp.png` beside the source.
- `:set editor image` on the source closes both windows.
