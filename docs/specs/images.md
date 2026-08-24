# Images

`:e photo.png` put four hundred kilobytes of PNG through a rope and called it
text. The terminal bi runs in can quite often just draw the picture: kitty,
WezTerm, Ghostty and konsole all speak the kitty graphics protocol. bi asks,
and where the answer is yes, an image file opens as the image it is.

## Status

**Built.**

## An image is a `Content`, not a `Buffer`

The fourth variant, beside `Text`, `Tree` and `Results` — which is exactly
what `windows.md` asked the next pane kind to be: a variant and a compiler
error, not a second boolean. Everything the tree design rejected about
buffer-hood is rejected here again, and harder: an image has no rope, no undo,
no selections, no LSP, no syntax slot, and every one of those would be a
suppression clause on `Buffer`. What it has is a path, its pixels, and a
scroll position — window state, like a tree's expansion.

```rust
pub enum Content {
    Text(Text),
    Tree(Tree),
    Results(Box<Results>),
    Image(Img),
}

pub struct Img {
    pub path: PathBuf,
    /// Pixel dimensions, from the decode.
    pub width: u32,
    pub height: u32,
    /// RGBA8, row-major. The frontend's to draw however it draws.
    pub rgba: Vec<u8>,
    /// Top-left of the visible region, in pixels, both clamped.
    scroll: (u32, u32),
    /// What the frontend last gave this pane, in pixels — the same
    /// arrangement as `Window::height`: the frontend reports the room, the
    /// core scrolls within it.
    viewport: (u32, u32),
    /// One pixel step of `hjkl`, set by the frontend beside the viewport —
    /// one text row's worth, so "a little" means the same distance it means
    /// in a file. Square, because pixels are.
    step: u32,
    /// Stable per opened image, for a frontend that uploads pixels to the
    /// terminal once and refers to them by number after.
    pub id: u64,
}
```

The core decodes — the `image` crate, RGBA8, first frame of an animation —
because pixel dimensions are core facts (the status line and the scroll
bounds need them) and because every frontend wants the same bytes. A GUI
blits `rgba`; the terminal frontend encodes it once for the wire. Decoding at
open rather than at draw is what makes a corrupt file fail at `:e`, where
the fallback is honest: it opens as the text it always was, with the decode
error in the status line.

**Which files.** Extension, not content sniffing: `png`, `jpg`, `jpeg`,
`gif`, `webp`, `bmp`. A misnamed file fails the decode and falls back to
text, which is the same answer sniffing would have bought at higher cost.

**Whether the terminal can draw it is not the core's question.** The core
opens images as images unconditionally; the terminal frontend that cannot
draw one renders a placeholder line — name and dimensions, centered — which
is still strictly better than the rope full of noise it replaces. The old
behaviour survives only as the fallback for files that do not decode.

## The keys are the keys it already had

No `KeyMode::Image`, no keymap section, no bindings to learn or rebind. A
window holding an image dispatches through the **normal** keymap, and the
handful of commands that mean anything to a picture are re-read as pixels
by `Editor::apply`, ahead of the view lookup that would find no rope:

```
h j k l      scroll by one step (a text row's worth), counts multiply
gg  G        top, bottom
0  ^         left edge          $  g_      right edge
Ctrl-D/U     half a viewport down, up
Ctrl-E/Y     one step down, up
```

Everything else falls through to what it already did. `Ctrl-W` anything,
`:q`, `:e`, `Ctrl-^`, `Ctrl-P`, `:ls`, the leader — none of them are
text commands, so none of them needed changing, which is the point: closing
an image and jumping out of its window are the keys you already press,
because the window is an ordinary window.

**`:bd` dismisses the image.** There is no buffer to delete, but the key
means "take this away" and refusing it on a technicality is exactly the
detached feeling this design exists to avoid. The alternate comes back if
the window has one that still exists; failing that, the most recent buffer.
The image is discarded rather than parked — that is what delete means, and
`Ctrl-^` is the key that parks.

**Modes do not exist here.** `i`, `v`, `R` and friends resolve against the
view and find none, so they were already inert; the mode-entering commands
that live on the session — `/`, `?`, `*`, `#`, `s`, `S` — are swallowed
for an image window instead. A search line over a picture is a promise the
pane cannot keep. The status row draws no mode segment for the same reason
it draws no `row:col`: neither fact exists.

## Centered, then scrolled

An image smaller than its pane sits centered, both axes. One larger scrolls,
and the motions above move the crop — small steps for `hjkl`, edges for the
line-and-column keys, which is the closest thing a picture has to lines and
columns. Scroll is held in pixels and clamped at both ends; the clamp
re-runs when the frontend reports a new viewport, so shrinking a window
never leaves the crop pointing past the edge.

Never scaled. A photo wider than the pane is cropped, not shrunk to fit —
scaling invents pixels or discards them, and the motions exist precisely so
the real ones can be looked at. (Fit-to-window is a zoom feature; zoom is
deliberately out until someone misses it.)

## The status row

The left half says `1920×1080` where a text pane says `12:40` — the size is
the fact about an image that position was about text. The name rides beside
it exactly as file names do, and the right half, which is the mode segment,
is absent. Unfocused panes swap the halves, as they always did.

## The wire

Terminal knowledge stays in the terminal frontend, `src/tui/graphics.rs`.

**Detection is a handshake, not an environment guess.** At startup, after
raw mode and before the event-reader thread takes stdin: send a kitty
graphics query (`ESC _G i=31,s=1,v=1,a=q,t=d,f=24 ; AAAA ESC \`) followed
by a primary device attributes query (`ESC [ c`). Every terminal answers
DA1, so the read cannot hang; a terminal that also answered `i=31;OK` speaks
the protocol. Stdin or stdout not being a tty means the question cannot be
asked. Environment sniffing (`$TERM`, `$KITTY_WINDOW_ID`) guesses wrong in
both directions over SSH, which is exactly where a Raspberry Pi gets used.

**tmux is the environment guess, because it has to be.** tmux answers DA1
itself and drops the graphics reply, so the handshake cannot see through
it. Instead tmux is asked about itself: `allow-passthrough` must be `on` or
`all` (the option arriving in tmux 3.3 — older tmux fails the query and the
answer is honestly no), and the attached client's `client_termname` must
name a terminal that speaks the protocol. Every escape then rides the
passthrough DCS (`ESC Ptmux;` … with inner escapes doubled), with the
cursor move *inside* the wrapper in outer-terminal coordinates — the pane's
position, asked of tmux when a placement is written, plus the cell. Cell
size falls back to tmux's `client_cell_width`/`height` where the pane's
own pty carries no pixel fields.

The honest caveat: the outer terminal does not know tmux windows exist, so
a placement drawn while you switch tmux windows stays on the glass until
bi next changes it. `allow-passthrough on` (rather than `all`) softens
this — tmux drops escapes from panes that are not visible — and the fix
proper is the protocol's Unicode-placeholder mode, which is deliberately
out until someone needs images and heavy tmux traffic at once.

**Cell geometry** comes from `window_size()` — the TIOCGWINSZ pixel fields —
re-read every frame because a font change mid-session is legal. Pixel fields
of zero mean the terminal never said, and images degrade to the placeholder
rather than to arithmetic on a guess.

**Transmit once, place per frame.** Pixels go up PNG-encoded (`f=100`,
direct transmission, 4KB base64 chunks) under the image's core `id`, once.
Each frame the renderer emits *placements*: image id, a placement id, the
source crop in pixels, the destination cell. Re-creating a placement with
the same placement id replaces it atomically, so moving or scrolling an
image is one escape sequence and no flicker. A placement whose window went
away is deleted by id; the uploaded pixels stay, because `Ctrl-^` is about
to want them and the terminal evicts its own store by quota anyway.

**Quitting takes everything down.** Leaving the alternate screen does not
delete a placement — the graphics layer is the terminal's, not the
screen's — so before the terminal is restored, every image bi uploaded is
deleted with its pixels (`d=I`, by bi's own ids, never another program's).
An editor that quits leaving a photograph floating over the shell has not
quit.

**The placement id is the window's id**, not a constant, because a bare
`:vs` clones the window — content, crop and all — and two panes then show
one image. Two placements of one image under one placement id are one
placement, and the first version of this keyed exactly that way: the second
split came up blank. One window, one placement id, however many of them
share the pixels.

**Placements ride at `z=-1`** — under text glyphs, above background fills.
The graphics layer is the terminal's own and ratatui cannot draw over it;
at the default z-index a placement floats over every cell, which put the
`Ctrl-P` picker *underneath* the photograph. At `-1` anything the renderer
draws in those cells — window-pick letters, a float that strays into the
pane — reads over the image. The picker gets more than that: a placement
that intersects the overlay is dropped for the frames the picker is up,
because text over a photograph is legible and still ugly; `sync`'s diff
deletes it and brings it back without any new mechanism.

The renderer draws blank cells under every placement — ratatui must own
every cell it thinks it owns — and the graphics module writes its escapes
directly to stdout after the frame, cursor saved and restored around them.
ratatui never learns images exist, which is what keeps the next backend
possible.

## Tests

- An image path opens as `Content::Image`; the buffer list gains nothing.
- A corrupt image falls back to a text buffer, error in the status.
- `hjkl` move the crop by a step and counts multiply; `gg`/`G`/`0`/`$` hit
  the edges; everything clamps at both ends.
- A viewport larger than the image pins the scroll at zero — centering is
  the frontend's, but the clamp is what makes it stable.
- Shrinking the viewport re-clamps an existing scroll.
- `i`, `v`, `/`, `s` in an image window leave the mode alone.
- `Ctrl-^` out of an image and back restores the same scroll.
- `:q` on a split closes the image pane like any pane.
- `:bd` on an image window brings back the alternate, or the most recent
  buffer when there is none — never a refusal.
- A bare `:vs` on an image makes two panes with independent crops, and both
  get a placement — the placement id is the window's.
- The picker's rectangle suppresses the placements it intersects, and only
  those, only while it is up.
- Quitting deletes every uploaded image from the terminal before the screen
  is handed back — nothing floats over the shell.
- The placeholder path: no graphics support still opens the image, and the
  status row still says the size.
