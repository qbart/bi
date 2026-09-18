# GUI

A second frontend, on [gpui](https://gpui.rs), Zed's GPU-accelerated UI
framework. `lib-split.md` made the core linkable so that this could exist;
this is the first thing to link it that is not a terminal.

## Status

**Built**, in its smallest possible form: one window, one pane, the focused
buffer's text in the theme's syntax colours, a cursor, two status rows, and
keys. Everything past that is listed under *Not yet* and is deliberately
absent rather than half-present.

## Why gpui

The core is already an editor with no opinion about pixels: `Editor::layout`
hands out rects in cells, `size_window` takes the room a pane got, `pane`
lends the buffer and its parse tree, and `Input::on_key` takes `bi::key::Key`,
which names nothing a terminal owns. What a GUI needs is a way to open a
window, measure a monospace font, draw coloured runs of text at cell positions,
and hear keys. gpui does those four things well, draws through the GPU, and is
Rust with no bindings layer. It is also large — the whole of Zed's UI stack —
which is the one cost, and the reason for the shape below.

## Shape

```
Cargo.toml          [workspace] members = ["gui"]; the root package is unchanged
gui/Cargo.toml      package bi-gui: bi by path, gpui
gui/src/main.rs     args, editor setup, the gpui application, one window
gui/src/keys.rs     gpui Keystroke → bi::key::Key
gui/src/view.rs     the Render impl: layout, key handling, the redraw timer, rows to runs
gui/src/cells.rs    a row as cells, and every pass that paints over one
src/config/xdg.rs   moved out of src/main.rs — see below
```

**A workspace member, not a feature.** A lib and a bin in one package share one
dependency list, so putting gpui behind an optional feature would still make
`cargo test --all-features` compile it, and every `cargo build` would resolve
its tree. As a workspace member the GUI is built only by `cargo build -p
bi-gui`, the terminal frontend's builds and tests are exactly what they were,
and `lib_boundary.rs` keeps its meaning: the library still cannot name a
terminal, and now it cannot name gpui either, because gpui is not in its
dependency list at all. The root stays a package rather than becoming a
virtual manifest so that `cargo build`, `cargo test` and `cargo run -- file`
at the root keep doing what the README says.

**The XDG config source moves into the library.** `XdgConfig`, `config_dir`
and the `.bi.toml` walk lived in `src/main.rs`, the terminal binary. Nothing in
them is about terminals: where `~/.config/bi` is and what `.bi.toml` means are
facts about the host, the same kind of fact as "language servers are child
processes", which `bi::lsp::transport::ProcessSpawn` already holds in the
library. A GUI that wants the same config and the same theme should not copy
sixty lines to get them. They become `bi::config::Xdg`, behaviour unchanged,
tests moved with them; `bi config init` and `bi config edit` stay in the
terminal binary because they are commands, not config.

## A frame

gpui renders when told to and when the window changes; there is no event loop
of bi's own. `View::render` does what `tui::render::render` does, in order:

1. **Cell size.** The font is the first of a short list of monospace
   families that fontconfig has — DejaVu Sans Mono, Liberation Mono, and so
   on — at 14px with a 20px line. A list rather than `monospace`, because
   gpui matches family names literally and knows no aliases. Its advance
   width is the cell width, so the window's pixel size becomes a grid of
   columns and rows exactly as a terminal's does.
2. **Layout.** `Editor::layout` with that grid, less one row for the footer.
   One window today, so one rect — but the call is the same one the terminal
   makes, and splits will need nothing new here. `size_window` reports the
   rect less its status row, which scrolls the pane to its cursor.
3. **Text.** One `Syntax::highlights` query for the visible byte range, as
   the terminal makes it, then per row: every char gets a *look* — the
   theme's style for its capture laid over the base foreground, in the terms
   a gpui text run takes: colour, background, weight, italic, underline —
   and the row is expanded to cells, tabs to their stops and control
   characters to `^X`, each cell keeping its char's look. Adjacent cells that
   look alike become one run, so a plain row is one run and a keyword costs
   three. The cursor is the cell under it with its colours swapped. In
   Command and Search modes the cursor goes to the footer instead, where
   typing goes.

   Over the syntax, in the terminal's order and for the terminal's reasons:
   decorations on the under layer, search matches, the selection — charwise,
   linewise filled to the pane's edge, or the block's own spans — the other
   cursors, decorations on the over layer, inline labels, then the
   horizontal scroll and the cursor-line fill. The terminal does this by
   splitting styled spans at every edge a highlight lands on; the GUI does
   it by painting cells, one per display column, which is the same
   arithmetic without the splitting, and lives in `cells.rs` where it is
   tested without a window. A cell knows its width — two for a CJK char,
   none for a combining mark — so the columns agree with the core's
   `display_col`, which is what a selection's edges are expressed in.
4. **Two status rows**, the terminal's arrangement: the window's row
   (`row:col  name [+]`) under the pane, and the footer, which is the `:` line,
   the search line, or the status message with the mode label pushed right.
5. **The redraw timer.** `Editor::redraw_in` says how long the frontend may
   sit before something on screen changes on its own — a yank's flash
   expiring, the checktime poll coming due. The terminal blocks its
   `recv_timeout` for that long; the GUI spawns a gpui timer that calls
   `notify`. The task is kept on the view and replaced every frame, and
   dropping a gpui `Task` cancels it, so there is never more than one pending.

Colour is the one translation. `theme::Color::Rgb` maps directly; the ANSI
names and the 256-colour indexes, which the terminal hands to the terminal,
here go through a fixed table — the xterm defaults — because a GUI has no
terminal palette to defer to. That function is the whole of what `gui/` knows
about colour that `tui/` does not, the mirror of `tui::render::color`.

## Keys

gpui delivers a `Keystroke { key, key_char, modifiers }`. `key` is the name
on the keycap (`"d"`, `"escape"`, `"enter"`, `"space"`), `key_char` is what
that press would have typed (`"D"` for shift-d, `None` under ctrl). The
translation is the mirror of `tui::keys::translate`:

- the named keys bi reads — escape, enter, backspace, tab, the arrows, home,
  end — by name; `space` is `Char(' ')`;
- otherwise `key_char` when it is exactly one character, so a shifted letter
  or symbol arrives as the character it produced, layout and all;
- otherwise `key` when *it* is one character — the ctrl case, where
  `key_char` is withheld because the typed character would be a control byte;
- anything else is dropped, which is where function keys and media keys
  already went in the terminal.

Modifiers copy across. A bare character carries its own shift, exactly as the
terminal reports it and as `config/keys.rs` documents.

The keys reach the editor by the terminal's path — `Input::on_key`,
`Editor::apply`, `Editor::settle`, then `notify` — and `session.quit` after a
settle quits the application. The window's close button quits too, without
the modified-buffer check `:q` makes; that check is an ex command's, and the
close button has not been taught to run one yet.

## `bi gui`

One command to remember, two programs on disk — git's arrangement, where
`git gui` is `git` finding and running `git-gui`. The terminal binary's
`gui` subcommand looks for `bi-gui` beside its own executable, then on
`PATH`, and execs it with the rest of the line unparsed. Nothing of gpui is
linked into `bi`: it knows a name, and if the name resolves to nothing the
error says so and names `make install-gui`, which builds `bi-gui` in release
and copies it beside `bi`. `make install` stays terminal-only, so a server
never builds a GPU stack for an editor it will run over SSH.

`gui` is a subcommand in every form, unlike `config` and `debug`, which
take two words: `bi gui` alone has to mean the window on an empty buffer.
A file actually named `gui` opens as `bi ./gui`.

The alternative — gpui behind a cargo feature, `bi gui` compiled in — was
rejected for the reason the workspace split exists: every `cargo build` of
the terminal would pay for the GPU stack, and `bi gui` would exist in some
builds and not others.

## What it does not wire

The hosts the terminal attaches on startup — clipboard, LSP, DAP, shell,
formatter, git baseline — are all left unattached. An editor whose frontend
never calls `set_lsp_spawner` attaches nothing, which is the design
`lib.rs` describes, and it is why this frontend is small: every one of those
is a line to add, not a subsystem to build. Config *is* loaded, through the
moved `Xdg`, because without it there is no theme and no keymap.

## Not yet

In roughly the order they will matter:

- more than one pane, and the tree, results, image and debug panes;
- the picker, the hover float, the completion menu, the signature float;
- the gutter: signs, numbers, indent guides, decorations;
- the hosts above, and a real clipboard through gpui's;
- `focus_gained` on window activation, for checktime;
- mouse: click to place the cursor, wheel to scroll.

## Verifying it without a screen

The machine this was built on has no display. What stood in for one:

```sh
Xvfb :98 -screen 0 1280x800x24 & openbox &
DISPLAY=:98 VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.aarch64.json \
  cargo run -p bi-gui -- file.rs
xdotool key j j j x colon w Return colon q Return
scrot shot.png
```

Three things matter there. `VK_ICD_FILENAMES` points Vulkan at lavapipe, the
software rasteriser, because gpui draws through Vulkan and a virtual
framebuffer has no GPU. `openbox` is not decoration: without a window manager
the window is mapped and drawn to and every screenshot is black, and gpui's
own `hello_world` example is black the same way. And `BI_GUI_LOG=1` puts
gpui's and blade's logging on stderr, which is how the first two were found.

## What this exposes about the boundary

The status rows' *text* — the name with its `[+]` and encoding badges, the
`row:col`, the footer's precedence of command line over search over message —
is computed inside `tui/render.rs`, and the GUI repeats a slice of it. That
text is not a terminal fact. The day the GUI draws a second pane it should
move into the core as something both frontends read, the way `Mode::label`
already did. The same goes for `cells`, the tab-and-control expansion: both
frontends want the same string for a row, and today both write the loop.
