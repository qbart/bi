# Opening in a split

Every list that opens a file — the tree, the file picker, the buffer
switcher, a results pane — opens it *here*: the handoff window, the focused
pane, wherever "here" is for that list. Usually right; but half the time you
are opening the second file to read it **beside** the first, and the dance is
always the same: open, `:vs`, `Ctrl-^` on one side. Three steps for one
intention.

## The keys

- **`s` in the tree** — the selected file in a new vertical split of the
  window the tree would have opened it in. The sidebar stays a sidebar; a
  directory just toggles, exactly as `Enter` would, because a directory is
  not a thing a window can show beside another. `Ctrl-V` is a synonym, so
  the split open is one gesture across the tree, the pickers and the
  results.
- **`Ctrl-V` in the pickers** — the file picker (`gf`, `Ctrl-P`) and the
  buffer switcher (`gb`, `:ls`) accept into a new vertical split. In every
  other picker — registers, symbols, themes, tree rows — it is plain
  `Enter`: those pickers choose things that have no window of their own, and
  a key that suddenly did nothing would read as a broken one. `Ctrl-Enter`
  is a synonym, where the terminal can send it at all (below); `Ctrl-V` is a
  plain control char, exists everywhere, and is telescope's own key for
  exactly this, so the hands arriving from vim already know it.
- **`Ctrl-V` in a results pane** — the match opens in a vertical split
  beside the results, cursor on the match, instead of displacing the pane
  the way `Enter` does. `Ctrl-Enter` again a synonym.

Vertical only, and no horizontal sibling: the intention this serves is
"beside what I am reading", and a second modifier for the rarer direction
can wait for someone to want it. `:sp <path>` exists.

## `Ctrl-Enter` needs the terminal's help

A legacy terminal sends `Enter` and `Ctrl-Enter` as the same byte, `\r` —
there is nothing bi can do about a difference that never reaches it. The
kitty keyboard protocol fixes exactly this, so at startup bi asks the
terminal whether it speaks it (`CSI ? u`, the protocol's own probe, which is
what crossterm's `supports_keyboard_enhancement` sends) and turns on the
**disambiguate** level — the mildest of the protocol's five, changing only
how ambiguous keys are encoded — for the session, popping it off on the way
out, panic hook included.

Where the terminal says no — and `Enter` therefore cannot have a chord on
it — nothing changes: `Enter` keeps opening things where they always
opened, and `Ctrl-V` and the tree's `s` still work, which is why the
*primary* spellings are a control char and a letter rather than the chord.
kitty, ghostty, wezterm, foot and alacritty all say yes; a tmux in between
answers for itself, usually no — which is what promoted `Ctrl-V` from
alternative to default.

The core never learns any of this: `Key { Enter, ctrl }` was always
expressible, and the probe lives entirely in the frontend
(`src/main.rs`), which is the one place that knows what a terminal is.

## What this is not

No `:set` to choose the split direction, no config for which lists take the
chord, no horizontal variant. One intention, one key per surface.

## Testing

Keymap: `s` in a tree maps to the split open, `Ctrl-Enter` maps to the
split accept in pickers and results, plain `Enter` everywhere unchanged.
Editor: the tree split opens the file in a second window and the sidebar
survives; the file picker and buffer switcher accept into a split; a
non-window picker treats the chord as `Enter`; a results-pane split leaves
the results showing with the cursor on the match in the new pane.
