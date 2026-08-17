# Config

bee has no config. The keymap is a set of `match` arms in `input.rs`, the
highlight colours are a `match` in `tui/render.rs`, and `:set` knows one option
because a real options table was waiting for this file. Nothing a user writes
can change any of it.

This adds a config layer: a TOML file the library parses, holding options, a
theme, and the keymap. `RECOMMENDATION.md` names this as the decision that gets
more expensive with every feature added before it, and README's "Next" has been
carrying it as a debt for three steps.

## Status

**Specified, not built.**

## What this is not

Not a scripting language. TOML holds data; a key binds to a *name*, and the
names are bee's own vocabulary. An embedded Lua or Steel would be a later
decision with its own spec, and this design is shaped so that it stays a
possible one: a script's contribution to the keymap would be a registered name,
which is a thing this design already has a slot for.

Not a plugin system, not per-project config, not a `:map` command. See
[Deliberately out](#deliberately-out).

## Where it lives

`$BEE_CONFIG`, else `$XDG_CONFIG_HOME/bee/config.toml`, else
`~/.config/bee/config.toml`.

```
~/.config/bee/
  config.toml
  themes/
    onedark.toml
```

XDG rather than `~/.bee.toml` because `themes/` needs a sibling, and so will
`queries/` when tree-sitter grows user grammars and `undo/` if `'undofile'` ever
lands. A dotfile in `$HOME` has nowhere to put any of them.

Resolution is the **frontend's** job — see [The boundary](#the-boundary).

## Every key belongs to a section

The top level of `config.toml` holds tables and nothing else. A bare top-level
key is a diagnostic, not a silent acceptance:

```
config.toml:3: `theme` is not in a section — did you mean [ui] theme?
```

```toml
[ui]
theme = "onedark"

[options]
number = 5
hlsearch = false

[keys]
leader = " "

[keys.normal]
"H" = "line_start"
```

`[options]` **is** the `:set` namespace — one key per `:set` option, spelled
identically, so `:set number 5` and `number = 5` are two ways to reach one
setting. `[ui]` holds what `:set` has no spelling for, which today is `theme`
alone.

The rule is arbitrary at the edges — `number` is arguably appearance and could
have gone in `[ui]` — but it is the only rule with no ambiguity: if `:set` can
say it, it is in `[options]`. Splitting by meaning instead would leave every new
option needing an argument about which half it belongs to.

## The user file is a patch

bee ships `src/config/default.toml` and compiles it in with `include_str!`. A
user's file is parsed as a patch over it: tables merge, keys override, and
`false` unbinds.

```toml
[keys.normal]
"H"          = "line_start"    # add
"<C-d>"      = false           # unbind; <C-d> now does nothing
"<leader>f"  = "picker_files"
```

The alternative — a user file that replaces the defaults wholesale — means every
person who wants one different key pastes 120 lines and then silently never
receives a binding bee adds later. That failure is invisible and permanent,
which is the worst combination.

This is also why `bee config init` writes the defaults **commented out**. See
[The CLI](#the-cli).

## The keymap

### Vocabulary is configurable, grammar is not

This is the load-bearing idea, and it is what keeps the file 120 lines instead
of thousands.

`input.rs` today hardcodes two different things and does not distinguish them:
that `w` means `Motion::WordForward`, *and* that `[count] operator [count]
motion` composes. Only the first is config.

**Grammar stays in code:**

- Counts, including that `0` is a count digit only when a count is already
  being typed.
- Operator + motion composition. There is no `dw` line anywhere in the config.
  Bind `w` to a motion and `dw`, `d2w`, `2d2w`, `c$`, `yG` and `vw` all follow.
- The doubled-operator rule that produces `Motion::CurrentLine` for `dd`, `cc`,
  `yy` — so rebinding `d` to `<leader>d` keeps `<leader>d<leader>d` working.
- `Motion::is_absolute`, which turns `5G` into `Line(5)` rather than five
  repeats of `last_line`.
- Whether a motion is exclusive, inclusive or linewise. That is `Motion::kind`,
  and it is a property of the motion, not of the key that reached it.

**Vocabulary is config:** which key produces which motion, operator, object or
action.

Helix's flat key-trie has no operator grammar to lean on, so it must enumerate
combinations. bee has one already — decision #3, "motions are data, not
actions", did most of this work three steps ago — and this design spends it.

### What a name resolves to

```rust
pub enum Binding {
    Motion(Motion),        // "word_forward"
    Operator(Operator),    // "delete"
    Object(TextObject),    // "word"  — only reachable from [keys.object]
    Action(Action),        // "undo", "paste_after", … stored with count = 1
    Pending(Pending),      // needs a further keystroke before it means anything
}

pub enum Pending {
    Find { forward: bool, till: bool },  // f F t T  → Motion::FindChar
    Replace,                             // r        → Action::ReplaceChar
    Register,                            // "        → sets the Sink
    Object { around: bool },             // i a      → look up in [keys.object]
}
```

Counts never appear in the table: a `Binding::Action` is stored with `count: 1`
and the grammar patches it through one `Action::with_count(n)` covering the six
count-carrying variants. Characters never appear either — they arrive through
`Pending`, from the keystroke after the binding.

Names use underscores, matching the Rust variants they stand for. No hyphens
anywhere in the vocabulary.

### The trie replaces three pending flags, and only three

`Input` carries seven ad-hoc pending fields today (`src/input.rs:26-41`). The
trie eats exactly the ones that are *prefixes*:

| Field | Fate |
|---|---|
| `g_pending` | **gone** — `"gg"` is a two-key trie entry |
| `window_pending` | **gone** — `"<C-w>s"` is a two-key trie entry |
| `delete_pending` | **gone** — the tree's `dd` is a two-key trie entry |
| `find_pending` | stays, driven by `Pending::Find` rather than hardcoded `f`/`F`/`t`/`T` |
| `replace_pending` | stays, driven by `Pending::Replace` |
| `quote_pending` | stays, driven by `Pending::Register` |
| `object_pending` | stays, driven by `Pending::Object` |

The split is the design working. The three that died were lookup problems; the
four that survived are waiting for *data*, which is a grammar problem. If a
fourth had dissolved into the trie it would mean the config language had eaten
something that belongs in the grammar.

### Structure

```rust
pub struct Keymap {
    normal: Map, visual: Map, operator: Map, object: Map,
    insert: Map, replace: Map, tree: Map, pick: Map,
}
```

For ~120 bindings a `HashMap<Vec<Key>, Binding>` beside a `HashSet<Vec<Key>>` of
live prefixes is enough. A real trie changes no interface and can come later if
it ever measures.

Maps are chosen by `Mode` and `ContentKind`, exactly as `Input::on_key` already
dispatches. Two rules keep the maps small:

- **Command and search lines are not in the table.** Those are literal text
  entry; there is nothing to bind.
- **The insert and replace maps bind only non-printable keys.** Any printable
  char with no binding inserts itself, which is why `[keys.insert]` is three
  lines rather than an enumeration of Unicode.

### Ambiguity: the first complete match fires, no timers

A sequence accumulates until it matches a binding, then fires. If it is a
prefix only, it waits. If it is *both* — a complete binding and the start of a
longer one — it fires, and the longer one is unreachable. The loader says so:

```
config.toml:14: "gd" is unreachable — "g" on line 12 already fires
```

Vim resolves the same case with `timeoutlen`. Rejected: a clock in the input
path makes keystroke handling untestable without fake time, and it is the source
of vim's most-complained-about input behaviour — the pause before `j` moves
because something might follow it. bee's defaults contain no such pair, and a
user who creates one is told at load rather than discovering it as lag.

### Notation

`<C-x>` ctrl · `<A-x>` alt · `<S-Up>` shift · `<Esc>` `<CR>` `<Tab>` `<BS>`
`<Space>` `<Home>` `<End>` `<leader>`.

Bare characters carry their own shift — `K`, not `<S-k>` — matching how
terminals and `KeyCode::Char` already report them. `<S-…>` is for the named keys
where shift is not folded into the character, which is what `<S-Up>` needs
today.

This parses into `key.rs` unchanged: `Mods` already carries `alt` and `shift`
unread, with a comment saying they are there for exactly this.

### Names live in one table

`src/config/names.rs` holds a single `&[(&str, Binding)]`, read by the parser.
It is also what powers

```
config.toml:7: unknown command: move_dwon — did you mean move_down?
```

and what a `:map` introspection command would print if one is ever added.

### The default keymap

`src/config/default.toml`, compiled in. Abridged; the full file is the shipped
default and is the parser's largest test.

```toml
[keys]
leader = " "

[keys.normal]
"h" = "left"
"l" = "right"
"j" = "down"
"k" = "up"
"w" = "word_forward"
"b" = "word_backward"
"0" = "line_start"
"^" = "line_start"
"$" = "line_end"
"gg" = "first_line"
"G" = "last_line"

"f" = "find_forward"
"t" = "till_forward"
"F" = "find_backward"
"T" = "till_backward"
";" = "repeat_find"
"," = "repeat_find_reverse"

"d" = "delete"
"c" = "change"
"y" = "yank"

"i" = "insert"
"a" = "insert_after"
"I" = "insert_line_start"
"A" = "insert_line_end"
"o" = "open_below"
"O" = "open_above"
"v" = "visual"
"V" = "visual_line"
"<C-v>" = "visual_block"
"u" = "undo"
"<C-r>" = "redo"
"p" = "paste_after"
"P" = "paste_before"
"." = "repeat"
"r" = "replace_char"
"~" = "toggle_case"
"J" = "join_lines"
'"' = "register"
"<C-e>" = "scroll_line_down"
"<C-y>" = "scroll_line_up"
"<C-d>" = "scroll_half_down"
"<C-u>" = "scroll_half_up"
"<S-Down>" = "move_lines_down"
"<S-Up>" = "move_lines_up"
"/" = "search_forward"
"?" = "search_backward"
"n" = "search_next"
"N" = "search_prev"
"*" = "search_word_forward"
"#" = "search_word_backward"
":" = "command"
"<C-p>" = "picker_files"
"<C-w>s" = "window_split_below"
"<C-w>v" = "window_split_right"

[keys.visual]
"o" = "swap_ends"
"O" = "swap_corners"
"d" = "delete_selection"
"y" = "yank_selection"
"i" = "inner"
"a" = "around"
"<C-n>" = "cursor_next_match"
"<Esc>" = "normal"

[keys.operator]
"i" = "inner"
"a" = "around"

[keys.object]
"w" = "word"
"W" = "big_word"
"p" = "paragraph"
"(" = "delimited_paren"
"{" = "delimited_brace"
"[" = "delimited_bracket"
'"' = "quoted_double"
"'" = "quoted_single"

[keys.insert]
"<Esc>" = "normal"
"<CR>" = "insert_newline"
"<BS>" = "backspace"

[keys.tree]
"y" = "tree_copy"
"x" = "tree_cut"
"p" = "tree_paste"
"<CR>" = "tree_open"
```

### Room for what does not exist yet

```toml
[keys.normal]
"<leader>gb" = "git_blame"
"gd"         = "lsp_definition"
```

Today both fail at load with `unknown command`, naming the line. That is the
correct behaviour and it is the whole argument for binding keys to names rather
than to other keys: `git_blame` has no keystroke to expand from, so a
key→keys mapping could never reach it. Whatever registers names later — git,
LSP, a `:command`, an embedded script — plugs into the same table, and the
keymap never learns what any of them are.

## Theme

`ui.theme` names a file in `~/.config/bee/themes/`, falling back to a built-in
of the same name, falling back to `default` with a diagnostic.

```toml
# themes/onedark.toml
[syntax]
keyword   = "#c678dd"
function  = "#61afef"
type      = "#e5c07b"
string    = "#98c379"
comment   = "#5c6370"
constant  = "#d19a66"
operator  = "#abb2bf"

[ui]
cursorline = { bg = "#2c323c" }
selection  = { bg = "#3e4451" }
search     = { bg = "#4a4520" }
cursor_alt = { bg = "#c678dd" }
tree_dir   = "#61afef"
tree_link  = "#56b6c2"
mark_copy  = "#e5c07b"
mark_cut   = "#e06c75"
```

`[syntax]` keys are the capture names `syntax.rs` already emits — `keyword`,
`function`, `type`, `string`, `comment`, `constant`, `attribute`, `operator` and
the aliases beside them at `tui/render.rs:152`. `[ui]` keys are the constants
above it: `CURSOR_LINE_BG`, `SELECTION_BG`, `EXTRA_CURSOR_BG`, `SEARCH_BG`,
`TREE_DIR`, `TREE_LINK`, `MARK_COPY`, `MARK_CUT`.

### Colours are bee's own type

```rust
pub enum Color { Ansi(Ansi), Indexed(u8), Rgb(u8, u8, u8) }

pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}
```

The frontend converts to `ratatui::Style`; a GUI converts to whatever it draws
with. This is decision #6 held to: the core says `keyword`, never
`ratatui::Color::Magenta`. `key.rs` made the identical move for keys and gives
the precedent.

### Three spellings, on purpose

- `"magenta"` — one of the sixteen ANSI names, which **respects the user's
  terminal palette**.
- `"color236"` — 256-colour index.
- `"#c678dd"` — 24-bit.

A bare string is shorthand for `{ fg = … }`; the table form adds `bg` and
attributes.

Hex-only was rejected: a user with a carefully-tuned solarized terminal would
get bee's idea of green instead of theirs, and it needs truecolor to look right
at all. The shipped `default` theme is ANSI, and reproduces today's colours
exactly — installing this step changes nothing on screen until a theme is
chosen. Shipped themes may be hex.

## Options

`[options]` today holds what `:set` already understands, and nothing invented:

```toml
[options]
number   = 5      # 0 off, -1 relative, N every Nth — see docs/specs/number.md
hlsearch = false
```

Both already exist as `Session::line_numbers` and `Session::highlight_search`.
`set_option` in `editor.rs:2203` — the match arm with the comment about waiting
for a config layer — becomes a lookup into the same options table the file
parses into, so a new option is one entry rather than one arm plus one parse
rule.

## The boundary

The library owns the config **types and the parser**. The frontend owns **where
the file is**.

```
src/config/mod.rs        Config { options, keys, theme }, ConfigSource, merge
src/config/parse.rs      toml -> Config, with line-numbered diagnostics
src/config/names.rs      &str <-> Binding
src/config/default.toml  include_str!
src/theme.rs             Theme, Color, Style

src/main.rs              $BEE_CONFIG / XDG resolution, file IO, the CLI
```

The keymap is editor semantics, so it belongs in the library — the argument
`key.rs` already makes for `Key`. A second frontend must not re-implement the
parser, the trie, the merge rules and the diagnostics. But "where does a config
file live on this platform, or inside this embedding host" is exactly what
varies per frontend, so that half stays out.

The library gains a `toml` dependency. `tests/lib_boundary.rs` is unaffected: it
proves the core is frontend-free by linking it and never naming a terminal, and
`toml` is not one. That test does change in one way, and the change is a
feature — `Input::on_key` gains a `&Keymap` argument, so the embedder in that
file now demonstrates supplying a keymap.

### Ownership

`Editor` holds the `Config`. Three unrelated consumers need parts of it —
`Input` the keymap, `render.rs` the theme, `Session` the options — and only
`Editor` outlives all three.

`Input` stays frontend-held and stateless with respect to config, taking
`&Keymap` as a fourth argument to `on_key`. A GUI frontend then reads
`ed.config().theme` and parses nothing.

### `ConfigSource`

```rust
pub trait ConfigSource {
    fn config(&self) -> Result<Option<String>>;             // None = no user file
    fn theme(&self, name: &str) -> Result<Option<String>>;  // None = try built-in
}

impl Editor {
    /// Applies a config source and remembers it for `:reload`.
    pub fn load_config(&mut self, source: impl ConfigSource + 'static) -> Vec<Diagnostic>;
}
```

A trait rather than a path, so the library never learns what a filesystem is and
an embedder can serve config from a database or a bundled resource.

Applied after construction rather than passed to `Editor::open`, which leaves
the existing `Editor::open` / `Editor::empty` call sites — three dozen of them,
nearly all tests — alone, and lets an embedder that wants no config simply
never call it.

```rust
let mut editor = Editor::open(path)?;
let problems = editor.load_config(XdgConfig::new());
```

## Errors are non-fatal

```rust
pub struct Diagnostic {
    /// 1-based, into whichever file it came from — config or theme.
    pub line: usize,
    pub message: String,
}
```

An unknown command name, unknown option, bad key notation or unreachable
binding drops **that binding**, records a `Diagnostic` with a line number, and
loading continues. Only malformed TOML falls back wholesale — and even then bee
starts on defaults.

An editor you cannot launch because of a typo in its config is an editor you
cannot use to fix the typo.

Diagnostics surface in the status line at startup — `3 config problems` —
rather than on stderr, which the alternate screen swallows.

## `:reload`

Re-reads the config and the selected theme through the same `ConfigSource`, then
swaps options, keymap and theme together. Startup and `:reload` run the same
code path, which is the only way the two stay in agreement.

**A failed reload changes nothing.** Malformed TOML reports the error and keeps
the running config. Reloading yourself into an unusable keymap, with no way to
type `:reload` again, is the one outcome worth engineering against. Success
reports `config reloaded`, or `config reloaded — 2 problems`.

`ExLine::Reload` already exists and means bare `:e` — re-read the *buffer* from
disk. It is renamed `ExLine::Revert`, a pure rename with no behaviour change, so
that two things called reload do not mean two different jobs. The spelling
`:reload` is free today; it falls through to `Unknown`.

## The CLI

Two subcommands, and no more.

**`bee config init`** creates `~/.config/bee/` and writes `config.toml` if
absent. If it exists: prints the path, exits 0, touches nothing. Never
automatic — a config file appears because you asked for one.

It writes the full default config **commented out**, under a header:

```toml
# bee config
#
# This file is a PATCH over bee's defaults, not a replacement. Anything left
# commented out keeps doing what bee does by default, including bindings added
# in future versions. Uncomment a line only to change it.

# [ui]
# theme = "default"

# [keys]
# leader = " "

# [keys.normal]
# "h" = "left"
# …
```

Writing them live would silently turn every user's file into a full
replacement, which is the failure the patch model exists to prevent. Commented
out, it is a self-documenting menu that is semantically empty.

**`bee config edit`** opens `~/.config/bee/` as a tree. `Editor::open` already
opens a directory as a tree (`editor.rs:947`), so this is argument routing and
no new editor code — and `themes/` is in the same tree. If the directory does
not exist: `no config yet — run \`bee config init\`` and exit 1. It does not
auto-create; init is the manual step.

`main.rs` treats `args[1]` as a path today. `config` is a subcommand only in the
two-argument form `bee config <sub>`, so `bee config` still opens a file named
`config`. Anything else after `bee config` is an error naming the two
subcommands.

## Rejected

**Keys bound to key sequences** (`"Y" = "y$"`, vim's `:nnoremap`). Composable and
needs no naming of internals, but it can only ever reference behaviour that
already has a keystroke — so a custom action, which by definition does not have
one, is unreachable. It also has no load-time validation: a typo is silently a
different edit. Named commands can gain expansion later as a single built-in
that feeds keystrokes back into the parser; the reverse is not true.

**Defaults built in Rust, config as an overlay.** Faster startup and defaults
that cannot be malformed, but it creates two ways to express a keymap with
nothing keeping them honest — the Rust default can quietly be more expressive
than the config language, and the gap arrives as a bug report. Parsing 4 KB of
TOML at startup is microseconds.

**The frontend owns config.** Keeps the library dependency-free, and makes a
second frontend re-implement the parser, the trie, the merge rules and the
diagnostics. That is the opposite of an embeddable core.

**`~/.bee.toml`.** No sibling for `themes/`, `queries/` or `undo/`.

**Project-local `.bee.toml`.** Genuinely useful for per-project indent and
grammar settings, and a real hazard: a cloned repo that rebinds keys is vim's
`exrc` problem, which needed a whole trust model, and it gets worse the day
scripting lands. The loader merges an ordered list of layers, so adding a
project layer later is a list entry and a trust decision, not a rewrite.

**`timeoutlen`.** See [Ambiguity](#ambiguity-the-first-complete-match-fires-no-timers).

**Hex-only themes.** See [Three spellings](#three-spellings-on-purpose).

## Testing

**The migration is differential.** The old hardcoded keymap is kept as a test
fixture for exactly one commit, and a sweep asserts that every key, in every
mode, with every count and operator prefix, produces the identical `Command`
through the trie as it did through the match arms. Then the fixture is deleted.
This is the `:m` sweep's method — 115 combinations, no disagreements — applied
to a refactor whose whole risk is silent behavioural drift.

**`default.toml` is the parser's largest test.** If the language cannot express
`f`'s pending argument, `<C-w>s`'s sequence or the object map, bee does not
start, and `cargo test` says so before a user does.

Beyond that:

- Unknown name, unknown option, bad notation, bare top-level key, unreachable
  binding — each yields a diagnostic with the right line number and no panic.
- Patch semantics: a user table adds without wiping its section; `false`
  unbinds; an unmentioned mode is untouched.
- `bee config init` is idempotent and never overwrites.
- `:reload` onto malformed TOML keeps the running config and reports.
- `:reload` picks up a changed `leader`, a changed binding and a changed theme.
- A theme naming a missing file falls back to `default` with a diagnostic.

## Order

Three steps, each useful on its own and each landing green.

1. **The layer.** `bee::config` types, the parser, diagnostics, `ConfigSource`,
   XDG discovery in `main.rs`, `[options]` wired to `Session`, `:reload`,
   `ExLine::Revert` rename, and both CLI subcommands. No keymap, no theme — but
   a real config file that does something, end to end.
2. **The theme.** `[ui] theme`, theme files, `Color` / `Style`, `render.rs`
   reading the table it currently hardcodes. Ships a `default` theme
   reproducing today's colours exactly.
3. **The keymap.** The names table, `Binding` / `Pending`, the per-mode maps,
   the keymap half of `default.toml`, the `input.rs` refactor, and the
   differential sweep that guards it.

The keymap is last because it is the only step that can silently change what a
keystroke does, and it is worth having the config layer proven before that.

## Deliberately out

- **Scripting.** The name registry is the slot it would fill; nothing here
  forecloses it.
- **`:map` / `:set` writing back to the file.** `:set` still changes the running
  value; persisting is a separate question about comment-preserving edits.
- **Per-project config**, and the trust model it needs.
- **A picker over themes**, or `:theme`. `ui.theme` plus `:reload` is enough to
  try one.
- **Options bee does not have.** No `tabstop`, no `ignorecase`, no
  `expandtab` — `[options]` holds what `:set` already understands, and grows
  when the features do.
