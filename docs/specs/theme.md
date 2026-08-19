# Theme

Every colour bi draws, named, in one place, replaceable from a file.

## Status

**Built.** Step 2 of `docs/specs/config.md` sketched this and
that sketch is superseded here — config.md keeps the `theme` *option* and
points at this file for what a theme is.

One thing config.md decided is reversed on purpose:

> The shipped `default` theme is ANSI, and reproduces today's colours exactly
> — installing this step changes nothing on screen until a theme is chosen.

The default is now **`gruvbox-dark`**, and installing this step does change
what you see. Today's ANSI colours survive as the `ansi` built-in, one
`:set theme ansi` away. The argument for ANSI-by-default was that it respects
a carefully-tuned terminal palette; the argument against is that an editor
with no opinion about its own colours has to be configured before it looks
like anything, and "looks right out of the box" is worth more than "matches
your terminal out of the box". Both are still available. Only the default
moved.

## The shape

Colour is **bi's own type**, never ratatui's:

```rust
pub enum Color {
    Ansi(Ansi),      // the sixteen names — respects the terminal palette
    Indexed(u8),     // 256-colour index
    Rgb(u8, u8, u8), // 24-bit
}

pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
}
```

This is decision #6 of RECOMMENDATION.md held to, and the same move `key.rs`
made for keys: the core says `keyword`, a frontend decides what `keyword`
looks like. A terminal converts to `ratatui::Style`, a GUI to a font weight
and an RGB triple it picks itself. `syntax.rs` has emitted capture names
rather than styles since it was written precisely so this file could exist
without touching it.

`reverse` is on the list because the focused window's status row uses it
today, and a theme that cannot say "reverse video" cannot reproduce what bi
already looks like. That is the bar for the `ansi` built-in.

### Three spellings, on purpose

- `"magenta"` — one of the sixteen ANSI names, which **respects the user's
  terminal palette**
- `"color236"` — 256-colour index
- `"#c678dd"` — 24-bit

A bare string is shorthand for `{ fg = … }`; the table form adds `bg` and the
attributes. Hex-only was rejected in config.md and stays rejected: a user with
a tuned solarized terminal should be able to write `"green"` and get theirs.
That the shipped default is hex does not make the other two spellings less
necessary — it makes them the reason `ansi` can exist at all.

## The lookup, and where it lives

`Theme::style(name)` takes a capture name and walks it down one dotted
segment at a time: `string.special.key` asks for `string.special`, then
`string`, then gives up and returns nothing. So `function.method` needs no
entry of its own, while a name that must differ from its own prefix — a JSON
key is a `string.special.key` and should not look like a string value — can
say so.

**That walk moves out of `tui/render.rs` and into the library.** It is editor
semantics, not terminal semantics: a GUI frontend needs the identical
fallback and would otherwise reimplement it, and the two would drift. What
stays in the frontend is one function from `theme::Color` to
`ratatui::Color`, which is the whole of what a terminal knows that a GUI does
not.

**A theme has to stop the walk where two things genuinely differ.** The walk
is what makes `function.method` free, and it is also what turns
`string.special.symbol` into a string if nothing says otherwise. Ruby is dense
with symbols — `:phases`, `class_name:`, `presence:` are on every other line —
so leaving it to fall through put 59% of a Ruby file in one green, alongside
the string literals, the regexes and the class names. That is exactly the
failure `docs/specs/tree-sitter.md` names for JSON keys, arriving at the other
end of the same pipeline: there the *query* had to be told the two captures
differ, here the *theme* has to be told the two colours do. Both gruvbox
themes therefore carry `string.special.symbol` and `string.special.regex`
explicitly, and a test asserts neither equals `string`.

`ansi` deliberately does not. It promises the colours bi had before it had
themes, and before it had themes these fell through.

Aliases are **explicit entries, not code**. Today `constructor` shares an arm
with `function` and `number` with `constant`, welded together in a match. In a
theme they are two keys that happen to hold the same value, and a theme that
wants constructors to differ from calls just writes a different one. The
built-ins list them out.

## `[syntax]`

Keyed by the capture names `syntax.rs` emits.

```toml
[syntax]
keyword     = "#fb4934"
function    = { fg = "#b8bb26", bold = true }
comment     = { fg = "#928374", italic = true }
```

## `[ui]`

Everything else on the screen. config.md named eight keys here; the screen has
**twenty-five** required colours in it plus the two optional ones below, and
eight of twenty-five is worse than none
— a theme that recolours the text and leaves a magenta picker border and a
blue mode badge behind produces a screen that looks broken rather than
themed. The list is therefore what is actually drawn, named for the role
rather than for the constant it replaces:

| key | what it paints |
|---|---|
| `background` | the frame, under everything — see below |
| `foreground` | text no capture claimed |
| `cursorline` | the line the cursor is on |
| `selection` | selected text |
| `search` | a search match |
| `cursor_alt` | every cursor but the primary one |
| `gutter` | the line-number column |
| `gutter_current` | the number on the cursor's own row |
| `rule` | the `│` between panes |
| `filler` | the `~` past the end of the buffer |
| `mode_normal` `mode_insert` `mode_pick` | the mode badge, per mode |
| `status` | the session message |
| `status_muted` | pending keys, the cursor position |
| `status_inactive` | an unfocused window's status row |
| `statusline` | the focused window's status row |
| `tree_dir` `tree_link` | directory and symlink rows |
| `mark_copy` `mark_cut` | a marked tree row |
| `picker_border` `picker_prompt` `picker_selected` `picker_badge` `picker_divider` `picker_preview` | the picker |

### The background is the theme's to claim

`background` is an `Option<Color>`, and the two cases are both first-class:

- **set** — bi paints it as the frame's base style before anything else
  draws, so every cell it does not otherwise touch is that colour.
  `cursorline`, `selection` and `search` keep layering on top exactly as they
  do now, because they were always painting *over* whatever was beneath them.
- **absent** — bi paints nothing, and the terminal's own background shows
  through. This is what bi does today.

A theme that ships a hand-picked palette generally wants the first: gruvbox's
greens and oranges were chosen against `#282828` and drift on anything else.
A theme built out of the sixteen ANSI names generally wants the second, since
the whole point of naming `green` is to get the terminal's green, and the
terminal's background comes with it. Neither is the "right" answer, so the
theme says which it is rather than a flag elsewhere deciding for it.

`ansi` omits it. `gruvbox-dark` sets `#282828`.

## Two names, and which one is live

`theme` names the palette; `ssh_theme` names the one used instead when the
session is remote, and defaults to `gruvbox-light`.

A second *name* rather than a modifier on the first, because the point is to
tell the two apart at a glance — a window editing files on another machine
should not look identical to one that is not, and the failure being designed
against is typing into the wrong one. The default pairing is dark locally and
light over the wire, which no local session gets by default, so the
distinction is visible without configuring anything. Setting `ssh_theme` to
the same value as `theme` turns it off.

`Options::active_theme(remote)` is the single place that chooses, so `:set`,
the config reader and the resolver cannot disagree about which name is live.
Over SSH that means `:set ssh_theme` is the one that moves the screen and
`:set theme` is the one that quietly does not — correct, and the reason the
status line echoes back the name it actually changed.

**The frontend decides whether a session is remote, and the library is told.**
`Editor::set_remote(bool)`; `main.rs` passes
`std::env::var_os("SSH_CONNECTION").is_some()`. Detecting it means reading the
environment, the environment is process-wide, and that is exactly what this
codebase already refuses to reach for from a testable path — `dir_from` takes
`$BI_CONFIG` and `$HOME` as arguments so that two tests running at once cannot
fight over them, and this is the same rule. An embedder that is a GUI or a
browser has no `SSH_CONNECTION` to consult and gets to answer for itself.

`set_remote` re-resolves if a config is already loaded, so it works before or
after `load_config`. That is not tidiness: `main.rs` has to pick an order, and
a rule that only works one way round is a bug waiting for the day someone
reorders two lines.

## Resolution

The theme name in force — `theme`, or `ssh_theme` when remote. In order:

1. `<config dir>/themes/<name>.toml`
2. a built-in of the same name
3. `gruvbox-dark`, with a diagnostic saying the named one was not found

A user file wins over a built-in of the same name so that `themes/gruvbox-dark.toml`
is how you adjust one colour of a shipped theme without forking it.

Reading it needs a second method on the trait a frontend already implements:

```rust
pub trait ConfigSource {
    fn config(&self) -> anyhow::Result<Option<String>>;

    /// `Ok(None)` means no such file — try a built-in. This is not an error.
    fn theme(&self, _name: &str) -> anyhow::Result<Option<String>> {
        Ok(None)
    }
}
```

It has a default so that an embedder with a config and no themes directory
keeps compiling, and gets the built-ins.

**The library owns the types and the parser; the frontend owns where the file
is.** Same boundary as config, for the same reason: "where does a theme live
on this platform" genuinely varies per frontend, and the library must not
learn what a filesystem is to serve it.

## Diagnostics

A theme is a config file and gets config's treatment: an unknown key, an
unparseable colour, a value of the wrong shape each drop that one entry,
record a line-numbered `Diagnostic`, and carry on. A theme with a typo in it
must still leave you an editor you can use to fix the typo — the argument is
identical to config.md's and so is the machinery.

`Diagnostic` already says which line. It does not say which *file*, and now
there are two. The message carries the theme's name so the two are told apart
without changing the type.

## The built-ins

Four, compiled in with `include_str!`, and parsed through the same parser a
user file goes through — so a malformed built-in fails a test rather than
being a second code path that cannot be wrong.

### `gruvbox-dark`

The default. From the palette in
[morhetz/gruvbox](https://github.com/morhetz/gruvbox), dark, medium contrast.

| | |
|---|---|
| keyword | red `#fb4934` |
| function, constructor | green `#b8bb26` bold |
| type, module, tag | yellow `#fabd2f` |
| string, escape, character | green `#b8bb26` |
| comment | gray `#928374` italic |
| constant, number, float, boolean | purple `#d3869b` |
| attribute, label | aqua `#8ec07c` |
| operator | orange `#fe8019` |
| punctuation, delimiter | fg4 `#a89984` |
| property, string.special.key | blue `#83a598` |
| background / foreground | bg0 `#282828` / fg1 `#ebdbb2` |
| cursorline / selection / search | `#32302f` / `#504945` / `#665c54` |

**Operators are orange, and gruvbox.vim says foreground.** That is a
deliberate departure. `Operator` linking to plain foreground is exactly the
bug `docs/specs/tree-sitter.md` describes under "A capture styled the colour
of nothing is a capture that did not happen" — `&x` and `x` are different
programs, and a palette that paints the `&` the colour of the text around it
is one where the capture may as well not have fired. Orange is gruvbox's own
and unused elsewhere in the table.

**Tags take the type colour, and before they took none.** `@tag` is what the
HTML and XML grammars call an element name, and no theme here had an entry for
it — so every `<div>` in an HTML file was parsed, captured and then painted the
same colour as the text around it. That is the operator bug again, one rung
further out: not a role given the wrong colour, but a role nobody remembered to
give one. It went unnoticed because HTML is mostly not tags; XML is *almost
entirely* tags, and a document that renders plain is hard to miss.

Yellow rather than a colour of its own, because an element name names a kind of
node — `<note>` and `<from>` are the same thing to a document that `struct` and
`enum` are to a program, and XML's own vocabulary calls them element *types*.
It also keeps the three parts of a tag distinct, which is what a reader is
actually separating: yellow name, aqua or blue attribute, green value. Sharing
a colour across roles is normal here — `constructor` is `function`'s, `module`
is `type`'s — and the aliases are written out per theme rather than welded
together in code, so a theme that wants tags in their own colour writes one
line.

### `gruvbox-light`

The same roles at the other end of the same palette, in gruvbox's *faded*
column: red `#9d0006` where the dark theme has `#fb4934`, green `#79740e`,
yellow `#b57614`, blue `#076678`, purple `#8f3f71`, aqua `#427b58`, orange
`#af3a03`, on `#fbf1c7`. The bright accents that carry a dark theme have
nothing to be bright against on a light background, which is why this is a
translation rather than a copy with the background swapped — and a test
asserts exactly that, by requiring each role to differ from its dark
counterpart. Comments are the one deliberate exception: gruvbox uses the same
neutral `#928374` at both ends.

**It is shipped and it is not the default**, which were always two decisions
rather than one. bi has no way to ask the terminal whether it is light —
there is no portable query for it, and guessing wrong is worse than not
guessing — so the light theme is one `:set theme gruvbox-light` away and the
dark one stays the thing you get.

### `pascal`

Borland Turbo Pascal 7, as an editor looked in 1992: light gray on `#0000a8`
blue, with a black-on-light-gray status bar and black-on-cyan selection.

**Every colour is one of the sixteen EGA/VGA text-mode colours**, and that
constraint *is* the theme. The look does not come from the particular blue; it
comes from there having been only sixteen of anything, and from a palette where
"bright" meant one bit. A nicer blue would make it a blue theme rather than
that blue, so a test walks the file and fails on any value that is not an EGA
colour — with one documented exception, `#000080` for the cursor line, which
the IDE had no equivalent of and which light blue is far too loud to serve.

**Keywords are white and calls are yellow**, which is the reverse of every
theme written since. That is what the screen did — `procedure`, `function`,
`begin` and `end` stood out bright from the body of a unit while the thing
being called was yellow — and flipping it to suit modern habit would be
missing the point. Strings and numbers share a green for the same reason: a
sixteen-colour palette did not spend two of them separating one kind of
literal from another.

It sets a background, obviously. A Turbo Pascal that is not blue is not Turbo
Pascal.

### `ansi`

Today's colours, exactly, in the sixteen names plus the two 256-indices the
current code uses. It exists to be a promise: **choosing a theme is not a
one-way door.** It is also the regression test — if `ansi` cannot express
what `render.rs` hardcoded, the type is missing something, and that is how
`reverse` got onto `Style`.

It sets no `background`.

## Testing

- every key in `Ui::REQUIRED` is set by both built-ins — a theme that forgets
  one is a hole on the screen, and the compiler cannot see it. This is the
  test that makes "twenty-five" a checkable number rather than a claim.
  `background` and `foreground` are excluded: they are exactly the two a theme
  is allowed to decline, and `ansi` declines both.
- the `ansi` built-in round-trips to the styles `render.rs` uses today. This
  is the one that says the door swings both ways.
- every built-in styles the roles that *carry* a file — `keyword`, `string`,
  `comment`, `type`, `property`, `tag` — because a capture with no entry falls
  off the end of the dotted walk and renders as plain foreground, which looks
  exactly like a grammar that never matched. `tag` is in that list by
  experience: it was missing from all four themes and took every HTML element
  name down with it.
- all three colour spellings parse, and a fourth thing does not
- the dotted walk: `function.method` finds `function`, `string.special.key`
  does *not* find `string`, an unknown name finds nothing
- a theme naming a colour that will not parse loses that key, keeps the rest,
  and reports the line
- an unknown `theme` name falls back to `gruvbox-dark` and says so
- a user `themes/<name>.toml` beats a built-in of the same name, and patches
  it rather than replacing it
- a theme name cannot escape the themes directory — it reaches the filesystem,
  so `../../etc/passwd` is not a theme
- `:set theme ansi` re-resolves rather than only moving the string: a name is
  not a palette, and a `:set` that reports success and changes nothing on
  screen is the failure worth engineering against
- both built-ins parse — via `Theme::default()` being the parsed default, the
  same trick `Config::default()` uses so the shipped file is exercised on
  every run rather than only in a test

## Deferred

**Detecting a light terminal.** `gruvbox-light` ships, so the remaining half
of that question is whether bi could *choose* it. OSC 11 asks the terminal for
its background colour and many terminals answer, but it is a query with a
timeout in the middle of startup and a wrong guess is worse than no guess.
Deferred rather than rejected.

**`:set theme` live-reload of an already-drawn frame** is free, because the
theme is read at draw time rather than baked into widgets. Noted because it
is worth *not* accidentally designing away.

**Per-language overrides.** A `[syntax.rust]` table is an obvious extension
and there is no demand for it yet.
