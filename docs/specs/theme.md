# Theme

Every colour bi draws, named, in one place, replaceable from a file.

## Status

**Built.** Step 2 of `docs/specs/config.md` sketched this and
that sketch is superseded here — config.md keeps the `theme` *option* and
points at this file for what a theme is.

One thing config.md decided is reversed on purpose:

> The shipped `default` theme is ANSI, and reproduces today's colours exactly
> — installing this step changes nothing on screen until a theme is chosen.

The default is now **`main`**, and installing this step does change what you
see. Today's ANSI colours survive as the `ansi` built-in, one
`:set theme ansi` away. The argument for ANSI-by-default was that it respects
a carefully-tuned terminal palette; the argument against is that an editor
with no opinion about its own colours has to be configured before it looks
like anything, and "looks right out of the box" is worth more than "matches
your terminal out of the box". Both are still available. Only the default
moved.

It moved twice. `gruvbox-dark` held it first and still ships unchanged; `main`
took it later and is described below. What that swap cost is one line in
`theme.rs` and one in `default.toml`, which is the point of the name being an
ordinary option — and every test that says "the default" says `DEFAULT_THEME`
rather than a colour, so the ones that had to change are exactly the handful
that pin what the default *looks* like.

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
differ, here the *theme* has to be told the two colours do. `main` and both
gruvbox themes therefore carry `string.special.symbol` and
`string.special.regex` explicitly, and a test asserts neither equals `string`.

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
| `search` | a search match — **both halves**, see below |
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

### `search` names a foreground as well as a background

Most of the highlights above are a background alone, and let whatever colour
the syntax gave the text show through. `search` cannot be, and this is the
rule rather than a preference: while `s` is aiming, everything on screen is
wearing `dim`'s foreground, so a match with no foreground of its own is a
match drawn in the colour of the text it is supposed to stand out from.
gruvbox-dark got this exactly wrong — `search` was a `#665c54` background and
`dim` was a `#665c54` foreground, so a match was painted its own colour and
disappeared.

`label` has the same job and the same rule, plus one more: it must not be the
*match's* colour either. `s` draws the letter touching the match it belongs
to, and one colour across both reads as one thing.

A test walks every built-in and checks all three: `search` sets both halves,
its background is not `dim`'s foreground, and `label`'s is neither.

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

`ansi` omits it. `main` sets `#161616`, `gruvbox-dark` `#282828`.

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
3. `main`, with a diagnostic saying the named one was not found

A user file wins over a built-in of the same name so that `themes/main.toml`
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

Sixteen, compiled in with `include_str!`, and parsed through the same parser
a user file goes through — so a malformed built-in fails a test rather than
being a second code path that cannot be wrong.

**They live in one table rather than three lists.** Adding a theme used to
mean a `const` for the file, an arm in `Theme::builtin`, and an entry in the
`BUILTINS` array — three places, with the failure mode being a theme that
`:set theme x` finds and the "built in: …" message does not. `BUILTINS` is now
a single `&[Builtin]` of `{ name, aliases, source }`, `Theme::builtin` scans
it and `Theme::builtins()` maps over it. That is the argument `Ui::set` makes
about a `[ui]` key existing for the parser and not for the drawing, one level
up.

**Some have a second name.** `gameboy` is `gb`, `molokai` is `monokai`,
`lighthaus` answers to `forest`, and each ported theme answers to the name its
source ships under — `xcodedark`, `shades-of-purple`, `github-dimmed`, `nord`.
A name is not a palette, so two names reaching one file costs nothing to keep
in sync. Only the primary is listed: an editor that answers "what themes are
there" with two spellings of one screen is answering a question nobody asked.
A test checks both halves of that.

### `main`

**The default.** A near-black frame with cool, desaturated accents, built out
of [IBM's Carbon](https://carbondesignsystem.com/guidelines/color/overview/)
palette — the colours behind the Nyoom.nvim screenshot this theme was asked
for. Two greys and eight accents, and nothing warm in it except the one place
warmth carries meaning.

| | |
|---|---|
| keyword | pink `#ff7eb6` |
| function, constructor | blue `#78a9ff` bold |
| type, module, tag | teal `#3ddbd9` |
| string, escape, character | green `#42be65` |
| comment | gray60 `#6f6f6f` italic |
| constant, number, float, boolean | purple `#be95ff` |
| attribute, label | light cyan `#82cfff` |
| operator, `string.special.regex` | magenta `#ee5396` |
| punctuation, delimiter | gray50 `#8d8d8d` |
| property, `string.special.key`, `.symbol` | cyan `#33b1ff` |
| background / foreground | gray100 `#161616` / gray20 `#dde1e6` |
| cursorline / selection | gray90 `#262626` / gray80 `#393939` |
| search | gray100 `#161616` on cyan `#33b1ff` |

**Why this is the default and gruvbox is not.** Nothing is wrong with gruvbox;
what changed is which look bi wants to be *its* look rather than a borrowed
one. gruvbox is a warm, high-chroma palette where the accents compete with
each other for attention — that is its charm, and it is also why a dense file
in it reads as colour first and structure second. Carbon was drawn for
interfaces rather than for terminals: it is a systematic ramp, the accents sit
at a similar lightness so no single role shouts, and the near-black frame
gives them all the same amount of room. An editor's default should be the one
you stop noticing.

**The frame is `#161616`, not the tint the screenshot shows.** A compressed
screenshot of a terminal over a dark page carries a blue cast that is not in
anybody's palette, and matching it would mean inventing a colour and then
being unable to say where it came from. Carbon's own gray100 is what the
screenshot is *of*, so that is what this theme takes. The rest of the
furniture walks the same ramp — `#262626`, `#393939`, `#525252`, `#6f6f6f`,
`#8d8d8d`, `#a8a8a8` — so every grey on screen is one step of one scale
rather than six separately chosen darks.

**Two colours are outside oxocarbon's sixteen, and both earn it.** `todo_warn`
is Carbon's yellow30 `#f1c21b`: the five TODO badges are the one place on
screen where the colour *is* the meaning, and a `WARN:` that is not warm reads
as a different word. `context` is cyan `#00d2ff`, and it breaks the furniture
rule on purpose — see below.

**`context` is loud here, and every other built-in's is quiet.** The rule in
`docs/specs/tree-sitter-context.md` is that the annotation marks structure
rather than naming anything, so it should be a shape you notice when you look
for it and not before. `gruvbox-dark` reads that as "one step below the
frame"; on a `#161616` frame the same reading gives `#0d0d0d`, which is not a
shape you notice when you look for it — it is one you cannot find when you go
looking, and an annotation that answers "what does this brace close" with
nothing may as well not be drawn. So `main` takes the other side of that
trade. Cyan rather than a Carbon value because it has to clear two bars at
once: legible against the frame, and not a colour a capture already owns —
`#33b1ff` is `property`, `#3ddbd9` is `type`, `#82cfff` is `attribute`, and an
annotation wearing any of them reads as code. A test pins both.

Unlike `pascal`, no test fences this theme into a palette. `pascal`'s
constraint *is* the theme; `main` is a theme that happens to have been drawn
from one, and pinning it would only make the next adjustment a fight.

### `gruvbox-dark`

From the palette in
[morhetz/gruvbox](https://github.com/morhetz/gruvbox), dark, medium contrast.
It was the default before `main` and is unchanged by having stopped being it —
one `:set theme gruvbox-dark` away, the same way `ansi` is.

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
| cursorline / selection | `#32302f` / `#504945` |
| search | bg0 `#282828` on yellow `#fabd2f`, gruvbox's own `Search` |

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

**It is shipped and it is not the default**, and it is what a remote session
gets — see `ssh_theme` above. bi has no way to ask the terminal whether it is
light —
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

### `vesper`

A near-black theme after [nexxeln/vesper.nvim](https://github.com/nexxeln/vesper.nvim),
sourced from that project's own `palette.lua` and `highlights.lua` rather than
approximated by eye.

**One warm accent and one cool accent carry almost everything.** `#FFC799`
(warm) is function, constructor, type, module, tag, constant, number, float,
boolean and label; `#99FFE4` (cool) is string and character. Keyword,
operator, punctuation, delimiter and attribute share a single muted gray,
`#A0A0A0` — which is not `foreground` (`#FFFFFF`), so it still clears the
operator test, but it is one flat gray doing five jobs where the other
built-ins spend five.

**No built-in styles bold or italic except this one's absence of both.** The
source's own `Comment` and `Function` definitions carry no attributes at all,
and neither does any other syntax group outside markdown markup — checked
directly against the file rather than assumed. Every other built-in here
bolds `function` and italicises `comment`; `vesper` does neither, on purpose,
because the source draws its hierarchy from colour alone.

**`escape` is the warm accent, not a shade of `string`.** That is the
source's own `@string.escape`, and it is easy to miss reading the palette
table alone: an escape sequence inside a string is meant to interrupt it, not
blend into the color around it.

**`property` is plain foreground**, because that is what the source's own
`Property` and `@property` groups are — not translated into a colour of its
own, which every other built-in here gives it.

Two adaptations depart from a literal reading, and both are called out in the
file: `filler` matches the source's choice to hide `~` entirely (painted the
background's own colour), but `statusline` does not — the source paints its
statusline the same colour as the background, and a role meant to be seen
cannot disappear into the one behind it, so this file lifts it one step. Where
the source has no equivalent at all — the mode badge, the five-way TODO
palette, a second cursor's colour — those roles are built from the same six
colours rather than introducing new ones.

### `gb`

A Game Boy's screen: dark forest green on a greenish cream LCD, after
[kungfusheep/mfd.nvim](https://github.com/kungfusheep/mfd.nvim)'s `mfd-paper`,
read out of that project's own `colors/mfd-paper.lua` and
`lua/lualine/themes/mfd-paper.lua` rather than sampled off a screenshot.
`:set theme gb`, or `gameboy`, which is the same theme under the name that
says what it looks like.

**It has one hue, and that constraint *is* the theme** — the same way sixteen
colours is `pascal`. A DMG had one green phosphor-lit LCD and four levels of
it, and this file has twelve levels of one green and nothing else: from
`#001008` at the near-black end, through `#002611` (the text), to `#BBC5B7`
(the frame) and `#C5CFC2` (a float). A test walks the file and fails on any
value whose green channel is not strictly the largest of the three, because a
single red error underline or blue keyword would not be a tweak to this theme,
it would be the end of it.

**So the hierarchy is weight, slant and underline rather than colour**, which
is the source's whole idea and the reason it can be monotone without being
flat:

| | |
|---|---|
| keyword | `#001008` bold — the one step darker than the text |
| function, constructor, tag, boolean, escape, label | fg bold |
| type, module | fg underlined — a type is a shape, and a shape gets a rule under it |
| string | fg *italic* |
| `string.special` (a JSON key, a Ruby symbol) | fg bold |
| `string.special.regex` | fg bold *and* italic — a string that is also special |
| comment | `#8A9A88` italic |
| everything else — constant, number, property, operator, punctuation | plain fg |
| background / foreground | `#BBC5B7` / `#002611` |
| search | `#BBC5B7` on `#002611` bold — the source's own `Search`, reversed out |

**The `string.special` rule survives here without a colour to spend on it.**
`theme.md` requires a symbol and a regex not to fall through to `string`, and
the reason is Ruby: 59% of a file coming out one green. This theme has one
green *everywhere*, so the fix cannot be a second one — instead `string` is
italic, `string.special` is bold, and a regex is both. A JSON file reads as
bold keys against italic values, which is the distinction the rule was asking
for. `gb` is therefore in that test alongside `main` and the gruvbox pair.

**The rule that operators must not be plain foreground is the one departure**,
and it is not one this theme can take. `docs/specs/tree-sitter.md` says a
capture painted the colour of the text around it is a capture that did not
happen — true, and in a theme where every capture is that colour it stops
being an argument about operators. The alternative is bolding `&`, `=` and `.`
on every line, which is worse than the disease. The source keeps `@operator`
and `@punctuation` plain and so does this.

**Where bi has furniture the source has none of, the ramp is spent rather than
extended** — vesper's policy, for vesper's reason. The five TODO badges are
the interesting case: with no hue to separate them, they are five rungs of the
ladder, ordered by how much each is entitled to shout. `FIX:` is cream on
`#001008` and `NOTE:` is dark on `#A5B2A2`, barely off the page; `WARN:`,
`TODO:` and `PERF:` sit between. The word in the badge already says which one
it is, so the paint only has to say how loud.

**`statusline` is bold, which no other built-in bothers with.** The focused
status row and an unfocused one share a background here (`#B0BAB0`, the
source's own `StatusLine` and `StatusLineNC`), so weight is the only thing
left to tell them apart — the same job `reverse` does in `ansi`. Two smaller
departures are marked in the file: `whitespace` is lifted from the source's
`Whitespace` to the comment green, because `theme.md` says a mark you have to
squint at answers nothing, and `git_delete` gains a bold on top of the
source's colour for the same reason it needed one — `git_add` and
`git_delete` are one ramp step apart, and the gutter glyph should not be
doing all the work alone.

### `lighthaus`

Greens and teals on a near-black frame, with one warm accent kept for the
things that are trying to get your attention. After
[lighthaus-theme/vim-lighthaus](https://github.com/lighthaus-theme/vim-lighthaus),
read out of that project's `colors/lighthaus.vim` and its lightline theme.
`:set theme lighthaus`, or `forest`, which is what it looks like and the name
it shipped under before being renamed to match its source.

| | |
|---|---|
| keyword, operator, label | cyan `#5AD1AA` |
| function, constructor, type, module, tag, boolean | teal `#47A8A1` |
| string, character | green `#50C16E` |
| comment | grey `#8E8D8D` |
| constant, escape | warm yellow `#FFEE79` |
| number, float | dusty pink `#D68EB2` |
| property, attribute, `string.special.key`, `.symbol` | magenta `#D16BB7` |
| `string.special.regex` | cyan `#5AD1AA`, the source's own `TSStringRegex` |
| punctuation, delimiter | `#CCCCCC` |
| background / foreground | `#18191E` / `#FFFADE` |
| search | `#18191E` on orange `#E25600` |

**Green where it paints code, warm where it paints attention, and a test
holds the file to it.** Every colour that paints *code* — keyword, function,
type, module, tag, string, character, boolean, operator, label — has more
green in it than red. Every colour that paints *attention* — a search match, a
yank flash, a jump label, the selection — has at least as much red as green.
That is the whole shape of the theme in two sentences: a green canopy, and
warmth only where something wants looking at. It is also the answer to why
this palette can afford `#E25600` at all. A
second test fences every value in the file to lighthaus's own twenty-odd `s:`
variables, the way `pascal`'s does the EGA sixteen, so "sourced rather than
sampled" stays true after the next edit.

**bi's furniture is other plugins' furniture, and lighthaus themed all of
them.** This is the built-in that needed the least invention: the picker is
`FZF_border`, `FZF_prompt`, `FZF_info` and `PmenuSel`; the four diagnostic
severities are the four `Coc*Highlight`/`Coc*Sign` groups; the three git signs
are `GitGutterAdd`, `GitGutterChange` and `GitGutterDelete`; `tree_dir` is
`NerdTreeDir`; the three mode badges are lightline's normal, insert and visual
rows. Where `vesper` had to build a mode badge out of a palette that had no
concept of one, this one could look it up.

**Comments are the departure, and it is the same argument gruvbox-dark's
orange operators make.** lighthaus paints `Comment` in `white2` `#CCCCCC`,
one step off plain foreground — so a comment reads at very nearly the weight
of the code it is explaining. That is "a capture styled the colour of nothing"
arriving from the other side: the capture fires, and the reader still cannot
tell prose from program. This file takes the palette's own `grey` `#8E8D8D`
instead, which lighthaus already uses for `LineNr` and `jsonQuote` and gives
to no syntax role, so the departure costs no new colour.

Three smaller ones, all marked in the file. `tag` takes the type colour rather
than vim's `Tag` group — vim's `Tag` means "you can press CTRL-] on this",
which is not what a tree-sitter `@tag` is, and the reasoning that put element
names on the type colour in `gruvbox-dark` applies unchanged. `rule`,
`indent_guide` and `whitespace` come from `non_text` rather than from the
source's own answers, because lighthaus draws its split as a filled dark bar
and its indent guides as filled columns, and bi draws both as a single `│`
glyph — a colour chosen to be a bar is either invisible or deafening as a
glyph. `whitespace` is then lifted one step to `grey` for the reason `gb`'s
was: a mark you asked for and have to squint at answers nothing.

**Selected text goes orange, which no other built-in does.** `selection` is
the source's own `Visual` — `#FF4D00` on `#090B26` — and it names a
foreground, so selecting a range repaints it rather than tinting what is
underneath. Every other theme here leaves the syntax showing through. It is
kept because it is the most recognisable thing about lighthaus on screen, and
because `#090B26` is darker than the frame: the selection reads as a hole cut
in the page with the text glowing in it.

### The eight ports

`kanagawa`, `xcode`, `purple`, `github`, `bonsai`, `monokai`, `nordark` and
`ferra` arrived together and are described together, because what is
interesting about them is mostly the same thing eight times: **how much of
bi's furniture a colorscheme built for Vim or Neovim actually has an answer
for, and what to do where it has none.**

Each file names its source in a `Based on <url>` line at the top, and the
palette's own variable names survive in the comments — `sumiInk3` and
`carpYellow`, `contrast` and `objectKeys`, `blush` and `ember` — because a
name says what a colour is *for* in a way `#FAD000` never will.

| | frame | the thing you notice |
|---|---|---|
| `kanagawa` | `#1F1F28` | ink-blue, muted; the `wave` variant |
| `xcode` | `#292a30` | salmon strings, dimmed punctuation; `xcodedark` |
| `purple` | `#2D2B55` | deep indigo, and comments in violet |
| `github` | `#22272e` | GitHub dimmed — the colours of a pull request |
| `bonsai` | `#151E23` | cool near-black with a green cast |
| `monokai` | `#1B1D1E` | Molokai: pink, chartreuse, sky, sand |
| `nordark` | `#2E3440` | Nord's polar night and frost |
| `ferra` | `#2b292d` | warm — the greys have red in them |

**The spread of how much was there to find is the interesting part.**
`bonsai` is the extreme at one end: it defines every treesitter capture bi
asks for, its own gitsigns and diagnostics, and a five-colour `@comment.*`
vocabulary that lands exactly on the five TODO badges — the only built-in here
that could look up all five rather than build any. `kanagawa` is close behind,
with a lualine theme for the mode badges and three of the five badges in its
own `@comment.error` / `.warning` / `.note`. At the other end, `github` is one
150-line file with no treesitter groups, no statusline, no diagnostics and no
git signs, so about a third of its furniture is built from the palette rather
than found; `monokai` predates treesitter entirely and takes no position on
any language, so bi's newer captures are placed by reading what it does with
the Vim group nearest in meaning. Everything built rather than found is
marked in the file it is built in.

**Four departures repeat across the ports, and each has the same cause.**

*A split drawn as a filled bar is not a split drawn as a glyph.* Vim colours
`VertSplit` knowing it will paint a whole column, so `kanagawa` makes it
*darker* than the frame and `ferra` makes it the frame's own colour — a
deliberately invisible seam. bi draws one `│`, and an invisible glyph is no
split, so those take the palette's "structure you are not reading" grey
instead. Same for indent guides, which several sources paint as filled
columns.

*A search match has to name both halves.* `github`'s `Search` is a bare
underline with no colours at all; `ferra`'s names a background one step off
the frame and leans on italic; `purple`'s and `xcode`'s are the same colour as
their own selection, so a match landing inside one disappears. In each case
the source's *other* search group — `IncSearch`, the one you are standing on —
does the job, and that is what `search` takes. bi has one search colour, and
the one worth having is the one you can find.

*A gutter number tuned against a gutter strip is not tuned against the frame.*
`github` and `monokai` and `kanagawa` all pair a fairly bright `LineNr` with a
darker background behind it. bi paints no gutter background, so the contrast
has to come out of the number.

*A sign needs a foreground.* `xcode` paints all three signify signs one blue,
which is three signs saying the same thing, so the gutter takes its `DiffAdd`
/ `DiffChange` / `DiffDelete` colours instead. `github`'s palette contains **no
green at all** — GitHub's dimmed diff colours live entirely in backgrounds —
so an addition there is blue, and that is the palette's doing rather than a
choice. `monokai`'s own `DiffDelete` foreground is a maroon meant to sit on a
dark red band and is unreadable alone in a gutter cell.

**Three departures are one-offs, and each is worth naming.**

`nordark`'s `dark` style — the plugin's default, and the only one of its six
that is actually Nord — has a variable named `red` whose value is `#ECEFF4`,
Nord's near-white *nord6*. Everything routed through it comes out the colour
of text. For its `Operator` that reads as a choice and is kept; for a cursor
block it is *better* than a real red and is kept gladly. For `GitSignsDelete`
it is not a choice: a deletion marked the colour of ordinary text, sitting
next to an addition and a change that both have colours, is a sign that does
not sign. That one takes the style's own `dark_red`.

`github`'s `warning` is a literal `#ff0000` — the one value in a theme called
*dimmed* that is not dimmed — and it lands on `ErrorMsg`. Worn by the text of
every diagnostic an LSP finds, it fights the frame rather than marking
anything, so `diag_error` takes the palette's coral, which is GitHub's own
danger colour and already the keyword.

`monokai`'s `StatusLine` is `guibg=fg`: a near-white bar with slate text, and
its most striking single choice. bi draws the git numstat on that row in
`git_add` / `git_change` / `git_delete`, and chartreuse and sand on `#F8F8F2`
are illegible — so the bar goes dark and the numbers stay readable. This is
the one departure here made for bi's sake rather than the source's, and a
three-line `themes/monokai.toml` puts the white bar back.

**Two places the ports read their source harder than it reads itself.**
`purple`'s generic `Function` group is the same orange as its keywords, but
every language-specific rule it ships — `jsFuncName`, `jsFuncCall`,
`jsArrowFunction` — paints a function `contrast` yellow; the generic group is
Vim's fallback rather than the scheme's intent, so the yellow is what this
takes. And `nordark`'s own `Tag` is the same green as its `String`, which
would render an XML document one colour from element name to attribute value —
the exact failure this spec records `tag` being added for — so it joins the
types, as `gruvbox-dark`'s does.

**No fences.** `pascal`, `gb` and `lighthaus` each get a test that pins the
constraint that *is* the theme — sixteen EGA colours, one hue, a green canopy.
These eight get no such test, because they have no such constraint: they are
palettes rather than arguments, and a fence around a palette only makes the
next adjustment a fight. What they get instead is one table asserting the
frame each claims, which is what a stray `#` anywhere else in the file would
most likely take down with it.

## Testing

- every key in `Ui::REQUIRED` is set by every built-in — a theme that forgets
  one is a hole on the screen, and the compiler cannot see it. This is the
  test that makes "twenty-five" a checkable number rather than a claim, and
  the one that a new built-in has to satisfy before it is a built-in.
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
- an unknown `theme` name falls back to `main` and says so
- a user `themes/<name>.toml` beats a built-in of the same name, and patches
  it rather than replacing it
- a theme name cannot escape the themes directory — it reaches the filesystem,
  so `../../etc/passwd` is not a theme
- `:set theme ansi` re-resolves rather than only moving the string: a name is
  not a palette, and a `:set` that reports success and changes nothing on
  screen is the failure worth engineering against
- every built-in parses — via `Theme::default()` being the parsed default,
  the same trick `Config::default()` uses so the shipped file is exercised on
  every run rather than only in a test
- `main` is the default, claims `#161616`, and keeps its greys on Carbon's own
  ramp — the theme-identity test each built-in gets, and the same shape as
  `pascal`'s
- `string.special.symbol` and `string.special.regex` are not just `string`,
  for **every** built-in but three. It used to be a hand-kept list of four
  names and is now the whole set minus its exemptions, so a new theme is in it
  by default rather than by remembering. The exemptions are `ansi` (which
  promises the colours bi had before it had themes, and before it had themes
  these fell through), `pascal` (sixteen colours, and it did not spend two of
  them telling one kind of literal from another) and `ferra` (which paints
  every `@string.special*` one rose — so a symbol still does not look like a
  string, which is what the rule protects, but a symbol and a regex look like
  each other, and inventing a colour the source does not have would be the
  port overruling it on a point it is entitled to). `gb` is the interesting
  member of the set that *is* covered: it has no second colour to separate
  them with, so it separates them with weight and slant instead, and the test
  cannot tell the difference — which is the point of it comparing `Style`s
  rather than colours
- `gb` is one hue from end to end — every `#rrggbb` in the file has strictly
  more green in it than red or blue. `pascal`'s test in a different palette,
  for the same reason: the constraint is the theme, and a single off-hue value
  would not be a tweak to it
- `lighthaus` is green where it paints code and warm where it paints
  attention — the two sentences the theme is a claim about, checked against
  the roles rather than asserted in a comment
- every value in `lighthaus` is one of that scheme's own `s:` variables, so
  "sourced rather than sampled" survives the next edit. `pascal`'s fence
  again, and `gb`'s, in a third palette
- every alias reaches the theme it is an alias of, and no alias is *also* a
  listed name — the two halves of the aliasing rule above, and the reason
  adding a spelling cannot quietly shadow a theme
- each of the eight ports claims the frame its source does, in one table.
  They get no fence of their own: see "No fences" above

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
