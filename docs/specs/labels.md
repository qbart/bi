# Labels

Three of the things left on TODO.md are the same machine wearing three hats:

- `Ctrl-W f` — put a letter on every window and go to the one you press
- `s` — type a few characters, put a letter on every match on screen, jump
- `S` — put a letter on every tree-sitter boundary around the cursor, select

Each of them collects targets, gives each a short unique name, draws those names
over the text without moving it, and reads one or two keys back. Written three
times that is three keymaps, three assignment schemes and three sets of
off-by-one bugs. Written once it is this.

## Status

**Built**, with window picking as the first client. `s` and `S` are the two
that follow, and they need only their own target lists.

## Assigning the letters

```rust
pub const KEYS: &str = "fjdkslaghrueiwotyvnmcxzqpb";
pub fn labels(count: usize, exclude: &[char]) -> Vec<String>;
```

**Home row first**, and under the strongest fingers first: `f` and `j` before
`d` and `k`, before `s` and `l`, before the rest. A label is a key you press
without looking, so the order is about the hand and not about the alphabet.

**Single characters while they last.** Past that, enough of the *worst* keys
become prefixes for two-character labels and the good keys stay single — so
the targets nearest the front keep the one-key labels, and nothing has to be
typed twice until there is genuinely no other way. Two characters cover 676
targets, which is more than a screen has rows.

**`exclude` is what makes `s` work.** When you have typed `fun` and the screen
holds `func`, the letter `c` cannot be a label: pressing it has to mean "narrow
to `func`", not "jump to the third match". The client passes the characters
that could extend a match and gets labels that avoid them. Nothing else needs
the parameter, and nothing else passes one.

## The mode

Labels are a mode, because while they are up every key means one thing:

```
Mode::Label     a letter picks; anything else cancels; Esc cancels
```

The typed prefix accumulates, so a two-character label is two presses with no
timeout anywhere — the same rule the keymap's sequences follow. A key that
cannot start any label cancels rather than being swallowed, which is what makes
a mistyped label cost one press instead of two.

State lives on the session beside the picker's, for the same reason: it is a
list, and a list has no business inside a `Mode` enum.

## Drawing them

An `Overlay` decoration per label (`docs/specs/decorations.md`) on the `Over`
layer — a letter you are about to press has to be readable wherever it lands,
including on top of a selection. The colour is the theme's `label`.

Because they are decorations, the frontend does not learn anything new: the
letters arrive in the same list as the indent guides and are painted by the
same code.

## Windows, the first client

`Ctrl-W f` — *focus* — labels every window at the top-left of its text area and
goes to the one you press.

**Not `<Tab>`**, which TODO.md asked for. `Tab` and `Ctrl-I` are the same byte
in a terminal, so `<Tab>` for window picking would silently take `Ctrl-I` —
buffer-next — with it. The window prefix is where every other window command
lives, and `Ctrl-W f` is one keystroke more than a bare `Tab` in exchange for
nothing being taken away. A `[keys.normal]` binding can spell it as anything,
`<leader>w` included.

Every window gets a letter, the focused one included: jumping to where you
already are is a no-op, and leaving it out would mean the letters move around
depending on where you are, which is exactly what a label is supposed not to do.

## Tests

- The first labels are `f`, `j`, `d` — the hand, not the alphabet.
- More targets than keys: the good keys stay single and the tail doubles up,
  and every label is unique.
- An excluded character appears in no label, not even as a prefix.
- Zero targets is no labels; more targets than two characters can name is as
  many as can be named, and it says so rather than losing them silently.
- `Ctrl-W f` labels every window including the focused one, and the letter
  lands at the top-left of each.
- Pressing a label focuses that window; pressing a key that is no label
  cancels; `Esc` cancels.
- A two-character label takes two presses and nothing in between.
