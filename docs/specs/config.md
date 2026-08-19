# Config

bi has no config. The keymap is a set of `match` arms in `input.rs`, the
highlight colours are a `match` in `tui/render.rs`, and `:set` knows one option
because a real options table was waiting for this file. Nothing a user writes
can change any of it.

This adds a config layer: a TOML file the library parses, holding options, a
theme, and the keymap. `RECOMMENDATION.md` names this as the decision that gets
more expensive with every feature added before it, and README's "Next" has been
carrying it as a debt for three steps.

## Status

**Step 1 built. Step 3 mostly built.**

The layer, `[options]`, `:reload` and both CLI subcommands ship. `[keys.*]`
loads and applies, and so do `[keys] leader`, multi-key sequences on both sides
of a binding, and the rules that make a prefix unambiguous without a timeout —
see "What step 3 actually shipped" below, which is honest about what is left.
The theme (step 2) has its own spec now — `docs/specs/theme.md` — and this
file keeps only the decision that `theme` is an ordinary option.

## What this is not

Not a scripting language. TOML holds data; a key binds to a *name*, and the
names are bi's own vocabulary. An embedded Lua or Steel would be a later
decision with its own spec, and this design is shaped so that it stays a
possible one: a script's contribution to the keymap would be a registered name,
which is a thing this design already has a slot for.

Not a plugin system, not per-project config, not a `:map` command. See
[Deliberately out](#deliberately-out).

## Where it lives

`$BI_CONFIG`, else `$XDG_CONFIG_HOME/bi`, else `~/.config/bi` — a
**directory**, holding `config.toml` beside `themes/`. An override that named
only the file would leave `bi config edit` and theme resolution with nowhere
to look.

```
~/.config/bi/
  config.toml
  themes/
    onedark.toml
```

XDG rather than `~/.bi.toml` because `themes/` needs a sibling, and so will
`queries/` when tree-sitter grows user grammars and `undo/` if `'undofile'` ever
lands. A dotfile in `$HOME` has nowhere to put any of them.

Resolution is the **frontend's** job — see [The boundary](#the-boundary).

## Every key belongs to a section

The top level of `config.toml` holds tables and nothing else. A bare top-level
key is a diagnostic, not a silent acceptance:

```
config.toml:3: `theme` is not in a section
```

Bare, with no suggestion, which is what ships. Now that `[options]` is the only
settings section, a `did you mean [options] theme?` hint is trivially correct
for any bare key and would be worth adding — it was not, while a second
settings section existed and the parser could not know which one you meant.

```toml
[options]
theme = "onedark"
number = 5
hlsearch = false

[keys]
leader = " "

[keys.normal]
"H" = "line_start"
```

`[options]` **is** the `:set` namespace, and there is no second settings
section. One key per option, spelled as `:set` spells it, so `:set number 5`
and `number = 5` are two ways to reach one setting — and so are `:set theme
onedark` and `theme = "onedark"`.

An earlier draft split appearance into a `[ui]` table and left `[options]` for
what `:set` understood. That put `theme` on one side and `number` on the other
for no reason a user could predict — both are things you set because of how you
want bi to look — and it meant every new option needed an argument about which
half it belonged to. One table has no edge to be arbitrary at.

It also removes a feature rather than adding one: with `theme` an ordinary
option, `:set theme onedark` is how you try a theme, and no `:theme` command
needs to exist. That obliged [`OptionValue`](#options) to grow a string
variant, which was a line of work rather than a design question, and cost it
`Copy` — one owned `String` beats a lifetime on a type both `:set` and the
TOML parser construct.

## The user file is a patch

bi ships `src/config/default.toml` and compiles it in with `include_str!`. A
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
receives a binding bi adds later. That failure is invisible and permanent,
which is the worst combination.

This is also why `bi config init` writes the defaults **commented out**. See
[The CLI](#the-cli).

## The keymap

### What step 3 actually shipped

`[keys.normal]`, `[keys.visual]`, `[keys.tree]` and `[keys] leader` load. A
binding is a command name or `false` to unbind, and **either side may be more
than one key**. What is missing is the half below: there is no `Binding` enum,
and `input.rs` still holds the default keymap as `match` arms.

Instead a name resolves to **the keys that already produce it**, and the user's
keys are rewritten to those at the top of `Input::on_key`. `"j" = "left"` makes
`j` arrive as `h`; `"<leader>g" = "goto_first_line"` makes Space then `g`
arrive as `g` then `g`.

The trade is deliberate. What it buys:

- The entire grammar keeps working without being touched. Rebinding `w` also
  rebinds `dw`, `d2w`, `c2w` and `vw`, because by the time the dispatcher sees
  the key it *is* `w`. A trie in front would have had to reimplement counts,
  operator-pending and the four argument-taking states before a single binding
  worked.
- Multi-key targets cost nothing extra: the target's keys are fed through the
  grammar one at a time, so `gg` reaches `Motion::FirstLine` through the same
  `g_pending` the typed keys use.
- It is small enough to be obviously correct, and it is guarded by tests that
  drive a real config through `Input`.

What it costs, and what still argues for the full design:

- **A name must already have keys.** `git_blame` cannot be bound, which is the
  exact case `config.md` uses to argue for names over key-to-key mapping. So
  the argument stands; this is a staging post, not a refutation.
- **The defaults are still in code**, not in `default.toml`.

Both disappear when `Binding` lands. Nothing here has to be un-built to get
there: the names table, the key notation parser, the sequence store and the
`[keys.*]` reader are all part of the final design — the store *is* the trie's
data, held as a map beside a set of live prefixes exactly as
[Structure](#structure) says it may be.

### A binding can be an ex line

```toml
[keys.normal]
"<leader>d" = ":bd<CR>"          # runs it
"<leader>w" = ":w<CR>"
"<leader>n" = ":set number 0<CR>"
"<leader>e" = ":e "              # prefills it, and waits
"<leader>m" = ":m "
```

A value starting with `:` is a **command line**, not a name. Everything `:` can
do is bindable the moment this exists — which is most of what a user actually
wants a leader for, and none of it was reachable before, because a name had to
be something bi already had keys for.

This is `Binding::Ex` from [What a name resolves to](#what-a-name-resolves-to),
arriving before the rest of that enum. It fits the eventual design unchanged:
the trie's value type was always going to be a `Binding`, and this is one of
its variants rather than a special case bolted beside it.

**The `<CR>` is mandatory, and it is what makes the line run.** Without one the
line is *prefilled* on the command line and left for you to finish, which is
the other half of what a binding is for: `":e "` puts you on an `:e ` line with
the path still to type. That is not a new mechanism — it is exactly how the
tree's `a` and `r` keys already work, filling in a `:create` or `:rename` line
for you to agree to.

An earlier draft stripped the `<CR>` as vim-habit noise, on the grounds that
the value is the command rather than the typing of it. That was wrong: it threw
away the one bit of information that distinguishes "do this" from "help me
write this", and the second is worth more for exactly the commands that take an
argument. Only the executed form is trimmed — a prefill's trailing space is the
whole point of writing one.

`":"` on its own is a legal prefill: it opens the command line, which is a
thing to want. `":<CR>"` runs nothing and says so.

It cannot be a key sequence, and that is deliberate: `":bd<CR>"` runs `bd`, it
does not "press `:`, then `b`, then `d`, then Enter". A binding that replayed
keystrokes would re-introduce the key-to-key mapping this design [rejected in
favour of names](#what-this-is-not) — with the added problem that
`Input::on_key` returns one command per key, so the mode would still be normal
when `b` arrived.

**The count does not repeat it.** `3<leader>d` runs `:bd` once. An ex line is
not a motion or an operator, and vim does not repeat one either.

Errors are the ordinary ones: a bad line reports on the status row exactly as
typing it would, because it *is* typing it — `run_ex` is the same entry point
the `:` line uses, which is the only way the two stay in agreement.

### Sequences, and the leader

**`leader` is a key, and it lives on `[keys]`.**

```toml
[keys]
leader = " "

[keys.normal]
"<leader>e" = "window_tree"
```

It takes one key in the same notation as any binding — `" "`, `"<Space>"`,
`"\\"`, `"<C-Space>"` — and defaults to `<Space>`, which `default.toml` ships
so that `<leader>` is spellable without configuring anything.

`<leader>` is expanded **at parse time**: the binding above is stored as the
two keys `<Space>` `e`, and nothing at runtime knows a leader was involved.
That is what makes changing one line re-point every binding that spells it,
which is the entire reason a leader exists. It also means `leader` is read
before the mode tables regardless of where in the file it sits — TOML is free
to hand back `[keys.normal]` first, and a leader that depended on file order
would be a trap.

**A prefix has no meaning of its own.** Binding `<leader>e` makes `<Space>` a
prefix, and `<Space>` stops being `Motion::Right` — there is no timeout to
decide between them. Vim resolves this pair with a clock, which is the source
of its most-complained-about input behaviour and is refused here. So the
decision is made at load, not at 500ms: a key that begins a binding is a prefix
and nothing else.

**A dead end drops the prefix, not the keystroke.** With only `<leader>e`
bound, typing `<Space>` then `j` discards the `<Space>` — it had no meaning of
its own to fall back to — and looks `j` up from the root, so it still moves
down. Swallowing both would lose a keystroke with nothing on screen to explain
it. The half-typed sequence shows in the status line beside the count and the
operator, for the same reason those do.

Note what this does *not* cover: `<Space>`'s built-in meaning is not a binding,
so shadowing it is not the ambiguous pair
[Ambiguity](#ambiguity-the-first-complete-match-fires-no-timers) is about. Two
*bindings* where one is the other's prefix still are, and still report — the
shorter fires and the longer is unreachable:

```
config.toml:5: "<Space>e" is unreachable — "<Space>" on line 4 already fires
```

**Taking over a key bi starts a command with is reported too.** `"gd" = …`
makes `g` the user's prefix, and a prefix has no meaning of its own — so `gg`,
`ge`, `gE` and `g_` stop resolving. The same for `<C-w>` and, in a tree, `d`.

```
config.toml:2: "gd" takes over "g", so word_end_backward, big_word_end_backward,
goto_first_line and last_non_blank can no longer be typed — bind them by name
to keep them
```

Not a refusal: taking `g` over is a thing a user may well mean, and every name
in that message can be bound straight back now that a target may be a sequence.
The list is derived from the multi-key entries in the names table, not written
out a second time, so it cannot drift from what bi actually has.

This is the one place the missing half is visible as more than a limitation.
When the defaults live in `default.toml`, `gg` and `ge` will be *bindings*, a
new `gd` will be their sibling in the same map, and the three of them will
coexist with nothing to report.

**Remapping stops at the first key of a command.** Once `r`, `f`/`F`/`t`/`T`,
`"`, `i`/`a`, `g`, `Ctrl-W` or a tree's `d` is waiting, the next key is that
command's **argument**, and an argument is not looked up in the keymap. Without
this rule `r<Space>` would type a leader instead of a space, and `f<Space>`
could not find one.

It is not a new rule so much as one this step is forced to state: it was
already wrong for single keys, where `"x" = "y"` quietly made `rx` write a `y`.
The trie design says the same thing in other words — a lookup starts at the
root, and every one of those states is already inside a production.

Counts and a pending operator are deliberately *not* on that list. After `d`
the next key is a fresh motion lookup, and that is exactly what makes a rebound
`w` also rebind `dw`.

**Multi-key names.** With targets able to be sequences, the names that were
absent because vim spells them with two keys now exist: `goto_first_line`,
`word_end_backward`, `big_word_end_backward`, `last_non_blank`, the tree's
`tree_delete`, `tree_first` and `tree_toggle_hidden`, and the window prefix —
`window_split`, `window_vsplit`, `window_close`, `window_only`, `window_tree`,
`window_cycle` and the four `window_focus_*`. Each is still a key sequence bi
already has; none is a new command.

Two rules that fell out of building it, both worth keeping:

- **Visual falls back to normal.** `input.rs` already falls through to `normal`
  for anything visual does not claim, so a motion rebound in `[keys.normal]`
  has to apply in visual too, or `v` then `j` would disagree with a bare `j`.
- **A tree falls back to normal for the keys it has no meaning for.** Three
  layers, in this order: `[keys.tree]`, then the tree's own vocabulary, then
  `[keys.normal]`. The middle layer is what stops `"j" = "left"` from meaning
  "collapse" in a pane sitting on a filesystem; the last is what makes
  `"<C-b>" = "window_tree"` close the sidebar it opened, because nothing in a
  tree spells `<C-b>`.

  The test is the *key*, not the length of the binding. A sequence-only rule
  stood in for this first — `<leader>e` was borrowed, a single key never was —
  and it got the common case wrong: one key bound to `window_tree` opened the
  tree and then could not put it away, because the second press happened with
  the tree focused. "Does the tree claim this key?" is the question that was
  always being asked, and `"j"` and `"<C-b>"` only answer it differently.

  A tree claims a key if its dispatcher has a meaning for it, prefixes
  included: `hjkl`, the arrows, `-`, `+`, `G`, `R`, `y`, `c`, `x`, `p`, `a`,
  `r`, `:`, `<CR>`, `<Esc>`, `<Tab>`, a count, `Ctrl-D`, `Ctrl-U`, `Ctrl-I`,
  `Ctrl-O`, `Ctrl-^`, and the `g`, `d` and `Ctrl-W` prefixes. Claiming the
  prefix claims the whole sequence: a normal-mode `"gd"` never fires in a tree,
  because `g` is already `gg` and `gh` there.

  What is borrowed still means whatever it means *here*: `goto_first_line` is
  `gg`, which is the tree's first row. The tree dispatcher remains an
  allowlist, so a borrowed binding for something a tree has no use for —
  `word_forward` — is a no-op rather than a way into normal mode.
- **Nothing is remapped in the modes that are text** — insert, replace, the
  command line, the search line, the picker. Rewriting a keystroke into another
  character is the one thing a keymap must never do to text being typed.

A third came out of a bug rather than a decision: `Input::reset` was
`*self = Self::default()`, which cleared the keymap along with the pending
count. A rebound key worked exactly once and then reverted. Pending state and
configuration share a struct, and `reset` means only the first.

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
combinations. bi has one already — decision #3, "motions are data, not
actions", did most of this work three steps ago — and this design spends it.

### What a name resolves to

```rust
pub enum Binding {
    Motion(Motion),        // "word_forward"
    Operator(Operator),    // "delete"
    Object(TextObject),    // "word"  — only reachable from [keys.object]
    Action(Action),        // "undo", "paste_after", … stored with count = 1
    Pending(Pending),      // needs a further keystroke before it means anything
    Ex { line: String, run: bool },   // ":bd<CR>" — built, ahead of the rest
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
because something might follow it. bi's defaults contain no such pair, and a
user who creates one is told at load rather than discovering it as lag.

### Notation

`<C-x>` ctrl · `<A-x>` alt · `<S-Up>` shift · `<Esc>` `<CR>` `<Tab>` `<BS>`
`<Space>` `<Home>` `<End>` `<leader>` · `<lt>` for a literal `<`.

Bare characters carry their own shift — `K`, not `<S-k>` — matching how
terminals and `KeyCode::Char` already report them. `<S-…>` is for the named keys
where shift is not folded into the character, which is what `<S-Up>` needs
today.

Anything not in a `<…>` group is one key, so a spelling is a sequence without
needing a separator: `gg`, `<C-w>s`, `<leader>gd`. A `<` that nothing closes is
the literal key — which is what `<C-w><` needs — and `<lt>` writes one where a
`>` later in the spelling would otherwise close a group around it. A group that
*does* close has to parse: `<Esk>` is a typo worth reporting, not five keys.

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

The `theme` option names a theme; **what a theme *is* moved to
`docs/specs/theme.md`** — the colour type, the `[syntax]` and `[ui]` tables,
resolution order, and the two built-ins.

What stays here is the decision that put it in `[options]` at all. `theme` is
an ordinary option, so `:set theme gruvbox-dark` and `theme = "gruvbox-dark"`
are two ways to reach one setting, exactly as `:set number 5` and
`number = 5` are. It also *removes* a feature rather than adding one: with
`theme` an ordinary option there is no `:theme` command to design.

Two things the theme spec settled that contradict what this file used to say,
recorded here so the difference is not a surprise:

- **The default is `gruvbox-dark`, not `ansi`.** This file argued the shipped
  default should reproduce today's colours exactly so that installing the
  step changed nothing on screen. It does change what you see now. Today's
  colours survive as the `ansi` built-in.
- **`[ui]` has twenty-five required keys, not eight**, plus `background` and
  `foreground`, which a theme may decline. The eight named here were the
  constants at the top of `tui/render.rs`; the rest of the screen — the mode
  badge, the status rows, the picker, the gutter — was hardcoded further
  down, and a theme that recolours the text and leaves those behind looks
  broken rather than themed.

`ConfigSource` grows a `theme(&self, name)` method for it, with a default
returning `Ok(None)` so an embedder that has no themes directory keeps
compiling and gets the built-ins. See `### ConfigSource` below.

## Options

`[options]` today holds what `:set` already understands, and nothing invented:

```toml
[options]
number    = 5      # 0 off, -1 relative, N every Nth — see docs/specs/number.md
hlsearch  = false
theme     = "gruvbox-dark"
ssh_theme = "gruvbox-light"   # used instead when SSH_CONNECTION is set
```

Both already exist as `Session::line_numbers` and `Session::highlight_search`.
`set_option` in `editor.rs:2203` — the match arm with the comment about waiting
for a config layer — becomes a lookup into the same options table the file
parses into, so a new option is one entry rather than one arm plus one parse
rule.

### `[filetype.<name>]`

A second *scope*, which is not the same thing as a second settings section:
the keys are the same keys, `[options]`'s, and what changes is which files they
reach.

```toml
[filetype.go]
tab_width = 4

[filetype.markdown]
expandtab = true
```

The name is the one `src/syntax.rs` gives a file — `rust`, `make`, `markdown`,
`csharp` — which is the same name its grammar is chosen by, because two tables
answering "what kind of file is this" would eventually disagree.

A section here is a *patch*: it names what it has an opinion about and says
nothing else. It is applied over `[options]`, over bi's own built-in table for
that type (which is what gives a Makefile its tabs whatever your `expandtab`
says), and under a `.editorconfig` and an explicit `:set`. The whole order,
and why it is that order, is `docs/specs/options.md`.

A value that no option would accept is reported against its own line and
dropped, exactly as it is in `[options]` — the check happens here, at parse
time, because a patch carries no line numbers to complain with later.

## The boundary

The library owns the config **types and the parser**. The frontend owns **where
the file is**.

```
src/config/mod.rs        Config { options, keys, theme }, ConfigSource, merge
src/config/parse.rs      toml -> Config, with line-numbered diagnostics
src/config/names.rs      &str <-> Binding
src/config/default.toml  include_str!
src/theme.rs             Theme, Color, Style

src/main.rs              $BI_CONFIG / XDG resolution, file IO, the CLI
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

Step 1 already has two copies of the options: `apply_config` writes them into
both `Session::options`, which `:set` mutates from then on, and `self.config`,
which `:set` does not touch. `Editor::config()` and `Session::options` can
disagree the moment `:set` runs. This is a known split, not an oversight —
fixing it means picking one owner for runtime state, and step 2 is the
natural point to do that, once the theme is a second consumer with the same
question to answer.

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
loading continues. Only malformed TOML falls back wholesale — and even then bi
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

**`bi config init`** creates `~/.config/bi/` and writes `config.toml` if
absent. If it exists: prints the path, exits 0, touches nothing. Never
automatic — a config file appears because you asked for one.

It writes the full default config with every **key** commented out, under a
header:

```toml
# bi config
#
# This file is a PATCH over bi's defaults, not a replacement. Anything left
# commented out keeps doing what bi does by default, including bindings added
# in future versions. Uncomment a line only to change it.

[options]
# theme = "default"
# number = 1

[keys]
# leader = " "

[keys.normal]
# "h" = "left"
# …
```

Section headers are written **live**, uncommented, even though every key
beneath one is commented out. An empty table parses to the same `Config` as no
table at all, so the file stays inert either way — but commenting the header
too would turn "uncomment a line" into a lie: the key would then sit outside
any table and the parser would correctly reject it as not being in a section.

Writing the keys live would silently turn every user's file into a full
replacement, which is the failure the patch model exists to prevent. Commented
out, it is a self-documenting menu that is semantically empty.

**The keymap half is generated, not written out.** `default.toml` holds the
options and the leader; the bindings are still `match` arms in `input.rs`, so
`config init` renders them from the [names table](#names-live-in-one-table)
instead of from a copy kept beside it. A hand-maintained list of ninety
bindings is a second source of truth, and the day it disagrees with `input.rs`
the file is worse than no file: it documents a keymap bi does not have.
Generated, every line is a binding the parser would accept, and a test
uncomments the lot and asserts exactly that — which is how the `"` register's
spelling was caught being written as `"""` rather than `"\""`.

It also settles what the shadowing diagnostic means. Binding `ge` takes `g`
over, and the listing binds `ge`, `gE`, `gg` and `g_` — so nothing is lost and
nothing is reported. The check therefore runs once per section, after every
binding is in, and ignores the sequences the same file binds back. Per line it
would have flagged bi's own defaults seventeen times.

**`bi config edit`** opens `~/.config/bi/` as a tree. `Editor::open` already
opens a directory as a tree (`editor.rs:947`), so this is argument routing and
no new editor code — and `themes/` is in the same tree. If the directory does
not exist: `no config yet — run \`bi config init\`` and exit 1. It does not
auto-create; init is the manual step.

`main.rs` treats `args[1]` as a path today. `config` is a subcommand only in the
two-argument form `bi config <sub>`, so `bi config` still opens a file named
`config`. Anything else after `bi config` is an error naming the two
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

**`~/.bi.toml`.** No sibling for `themes/`, `queries/` or `undo/`.

**Project-local `.bi.toml`.** Genuinely useful for per-project indent and
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
`f`'s pending argument, `<C-w>s`'s sequence or the object map, bi does not
start, and `cargo test` says so before a user does.

Beyond that:

- Unknown name, unknown option, bad notation, bare top-level key, unreachable
  binding — each yields a diagnostic with the right line number and no panic.
- Patch semantics: a user table adds without wiping its section; `false`
  unbinds; an unmentioned mode is untouched.
- `bi config init` is idempotent and never overwrites.
- `:reload` onto malformed TOML keeps the running config and reports.
- `:reload` picks up a changed `leader`, a changed binding and a changed theme.
- A theme naming a missing file falls back to `default` with a diagnostic.

## Order

Three steps, each useful on its own and each landing green.

1. **The layer.** `bi::config` types, the parser, diagnostics, `ConfigSource`,
   XDG discovery in `main.rs`, `[options]` wired to `Session`, `:reload`,
   `ExLine::Revert` rename, and both CLI subcommands. No keymap, no theme — but
   a real config file that does something, end to end.
2. **The theme.** The `theme` option, theme files, `Color` / `Style`,
   `render.rs` reading the table it currently hardcodes, and the string variant
   `OptionValue` needs to carry a theme name. Ships a `default` theme
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
- **A picker over themes.** `:set theme <name>` is enough to try one, and it
  falls out of `theme` being an ordinary option rather than needing a command
  of its own.
- **Options bi does not have.** No `tabstop`, no `ignorecase`, no
  `expandtab` — `[options]` holds what `:set` already understands, and grows
  when the features do.
