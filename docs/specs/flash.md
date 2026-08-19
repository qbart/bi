# A flash on what was yanked

`yy` prints nothing, changes nothing and moves nothing. It is the one command
whose entire effect is invisible, and the only way to find out whether you
yanked what you meant to is to paste it somewhere and look.

So the text lights up for a moment. Vim has nothing here; neovim grew
`vim.highlight.on_yank` and everybody turns it on.

## Status

**Built.**

## What it is

```
yank_flash = 150     milliseconds; 0 turns it off
```

One option rather than a switch and a duration, because "how long" and
"whether" are the same question — `yank_flash = 0` is a flash of no time at
all, which is the honest spelling of off.

Every yank, whatever its shape: `yy`, `yiw`, `y` over a selection, a blockwise
`y` (each row of the rectangle lights up), and `"+y` (the register it went to
changes nothing about what was read). Not delete, not paste, not change — those
three are visible in the text already, and a flash on top of them is noise.

The colour is the theme's `flash`.

## How it works

The yank records what it read: the char ranges, the buffer they were in, and
the moment the light goes out. `Editor::decorations` turns those into
`Repaint`s on the `Under` layer while the moment has not passed, and into
nothing after it — so the flash is a *derived* thing like every other
decoration, and no timer has to reach into a buffer to remove one.

```rust
impl Editor {
    /// How long until something on screen changes on its own.
    pub fn redraw_in(&mut self) -> Option<Duration>;
}
```

That is the whole of what a frontend has to learn. Its loop blocked on the next
key; now it waits for the next key *or* for that duration, whichever comes
first, and draws either way. `redraw_in` clears a flash that has already
expired, which is what stops "expired zero seconds ago" from being a timeout
the loop can spin on.

**The core reads the clock, not the frontend.** A clock is not a terminal —
a GUI has one, an embedder has one, and a headless test has one. What the
frontend owns is *waiting*, which is exactly what `redraw_in` hands it.

## The range a yank read

`Buffer::operate` and `operate_range` now hand back the char range they
covered, alongside the register entry and the landing cursor. The range was
already computed inside both — it is what `take` cuts or copies — and returning
it means the flash does not have to guess it back out of the entry's text,
which for a linewise yank is not even the same length as what was on screen.

Blockwise yank lights each row's span, because a rectangle is not one range and
painting the bounding box would say a rectangle was something else.

## What this does not do

**No flash on paste, delete or change**, as above. **No fade** — a terminal
cannot animate one without redrawing on a timer for the duration, which is a
lot of frames to spend saying something the first one already said. **Nothing
outside the buffer it happened in**: yank in one window, look at another
showing a different file, and there is nothing to see, because nothing there
was yanked.

## Tests

- A yank produces a flash decoration over exactly what it read, in the buffer
  it read it from.
- Charwise, linewise and blockwise: the last lights one range per row.
- `yank_flash = 0` produces nothing at all.
- An expired flash produces nothing, and `redraw_in` goes back to `None` once
  it has.
- Delete and paste produce no flash.
- Another buffer's window shows nothing.
