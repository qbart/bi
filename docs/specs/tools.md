# Tools for game development

`:tool tileset` was the first tool: a mode of one picture, driven from the
ex line and the normal keys, drawing its result through the kitty
graphics protocol. This is the list of the others worth having, and the
one observation that makes most of them cheap.

## Status

**Ideas**, with five built since: whole-image operations
(`image-ops.md`), the form any tool's knobs live in (`form.md`), the
normal map generator (`normalmap.md`), the curve editor (`curve.md`) and
the property view (`props.md`). Each entry that gets built gets its
own spec, and this list points at it.

## The enabler

A window already shows pixels the core produced — `Img::from_pixels` does
not care whether they came from a PNG or from a tool. Any tool that can
draw its result into an RGBA buffer gets kitty display, zoom, scrolling
and splits for free. Most of what follows is "draw into a buffer, then
let the picture machinery show it".

## 2D — the tileset's neighbours

- **Tilemap editor.** Paint tiles from a tileset into TMX or Tiled JSON
  layers, with the map rendered live. Collision and object layers stay as
  text edited beside it. The reason the sheet is a *tileset* and not a
  tilemap — see `tileset.md`.
- **Animation preview.** `:tool anim 8,1 fps 12` plays frames from a strip
  or an atlas in the window, onion-skinning the previous frame. Small on
  top of the tileset's selection; what sprite work spends its time on.
- **Palette tool.** Extract a sheet's palette, swap or remap colours, write
  indexed PNGs. Palette swaps for enemy variants are the classic pixel-art
  workflow no terminal tool does well.
- **Pixel pencil and fill** on the selection. Once a tile can be zoomed to
  16x and selected, drawing into it is a small step.
- **Bitmap font preview.** Render a string with a BMFont or glyph atlas so
  kerning and baseline problems show before the engine runs.

## 3D

- **Mesh viewer.** A software rasteriser drawing glTF or OBJ as wireframe
  or flat-shaded into an image, orbiting with `hjkl`; vertex and triangle
  counts, materials and the bounding box in the status row. The `gltf`
  and `tobj` crates parse; no GPU needed.
- **UV and texture inspection.** The UV layout drawn over the texture,
  which is how stretched seams get found.
- **Shader validation.** `naga` validates WGSL and GLSL offline, so shader
  errors arrive through the diagnostics pane the way LSP errors do. No
  preview needed for the errors to be worth having.

## Structured data

- **Curve and plot tool.** Render an easing curve, a damage formula or an
  XP table as a graph, move bezier handles with the cursor, write the
  numbers back. Tuning is where designers spend hours in spreadsheets.
  Built, editing code in place — see `curve.md`.
- **Property view.** Unity's inspector over a JSON dialect of typed
  structs and instances: every field on a row, defaults dimmed, enums
  cycled, refs followed across files, the schema edited the same way.
  Built — see `props.md` and the format in `bi-format.md`.
- **Table view** for CSV and TSV. Aligned columns, cell-wise navigation,
  sort and sum. Balance sheets and loot tables live in these files.
- **Graph preview.** A state machine, behaviour tree or dialogue script
  (Ink, Yarn) drawn as a laid-out graph. One renderer serves all three.
- **Hex view with struct overlays.** Save files and packed asset formats
  read far better with a layout spec naming the fields.
- **Localization audit.** Missing or untranslated keys across language
  files, reported through the results pane find-in-files already uses.

## Pipeline

- **Asset audit.** Files under the assets directory nothing references, and
  references that point at nothing. Find-in-files run backwards, reusing
  its machinery.
- **Waveform view.** A WAV or OGG as a waveform with loop-point markers,
  played through an external player. Cue sheets and loop trimming.

## Order

By payoff against effort: the animation preview first (a week of use for
a day of work), then the tilemap editor, then the mesh viewer as the
first 3D piece and the curve tool as the first data piece. The palette
tool slots in anywhere; it is small.
