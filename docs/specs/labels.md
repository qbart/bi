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

An `Inline` decoration per label (`docs/specs/decorations.md`) — the letter is
inserted *between* two cells and pushes the rest of the row right, rather than
sitting on top of a character and hiding it.

**Not an overlay**, which is what this was first, and the reason is the whole
point of a label: it names a place, and the place is a character. A letter
drawn over that character answers "press `f`" and takes away "for *what*" —
`return` reads as `geturn` and you are picking blind. One cell cannot hold two
characters, so the row gets wider for as long as the letters are up, which
costs a column at the right edge and is worth it.

Two labels wanting the same column get a cell each, in the order they were
produced. `S` is the client that needs this: two scopes can end in the same
place, and a letter that quietly wins is a scope you cannot see the extent of.

**A window label is the exception, and it proves the rule.** `Ctrl-W f` names
a *window*, not a character, so there is nothing under it that it is talking
about — it is an `Overlay` in the middle of the pane, three rows tall and two
cells wider than the letter, painted in `label`. A single character in a corner
is one more character on a screen full of them, and you end up hunting for the
thing that exists to save you hunting. Nine cells go missing for one keystroke;
the next one gives them back.

The colour is the theme's `label`. Because they are decorations, the frontend
does not learn anything new: the letters arrive in the same list as the indent
guides and are painted by the same code.

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

The letter is drawn as a block in the middle of the pane rather than at its
top-left, for the reason above. Centred on the middle of what the pane is
*showing*, and a row of the block that falls past the end of the file is not
drawn — a two-line file gets less of a block, which still reads, rather than
one painted over the `~`s where there is no line to decorate. The core knows
where the middle is because it knows the gutter width, which moved out of the
frontend for exactly this: how many digits the last line number needs is a
fact about the file and the options, not about a terminal.

## Tests

- The first labels are `f`, `j`, `d` — the hand, not the alphabet.
- More targets than keys: the good keys stay single and the tail doubles up,
  and every label is unique.
- An excluded character appears in no label, not even as a prefix.
- A label is inserted rather than drawn over: the character it names is still
  on the screen, one cell along.
- Two labels at one column are two cells, in the order they were produced.
- Zero targets is no labels; more targets than two characters can name is as
  many as can be named, and it says so rather than losing them silently.
- `Ctrl-W f` labels every window including the focused one, as a three-row
  block in the middle of each with the letter in its centre.
- A file shorter than the block keeps the rows it has.
- Pressing a label focuses that window; pressing a key that is no label
  cancels; `Esc` cancels.
- A two-character label takes two presses and nothing in between.
