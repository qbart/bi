# `s` — find on screen and jump

`/` is for finding something in a *file*. Most of the time what you want is
already on screen and three or four rows away, and `/` makes you type it, press
Enter, and then press `n` until you are there.

`s` is for that case: type a few characters, every match on screen gets a
letter, press the letter, you are there. One keystroke per step and no Enter
anywhere.

## Status

**Built.**

## What happens

```
s               everything on screen dims; nothing is matched yet
{chars}         every match in the viewport lights up, each with a letter after it
{letter}        the cursor goes to the start of that match
Esc             back to normal, cursor where it was
Backspace       take back a character; on an empty query, leave
```

The mode ends by itself when the query stops matching anything, because at that
point there is nothing to press and nothing to narrow — the next keystroke was
going to be `Esc` either way.

**The letters never shadow a narrowing keystroke.** With `fun` typed and
`func` on screen, `c` cannot be a label: pressing it has to mean "narrow to
`func`". That is what `label::labels`'s `exclude` list is for, and it is the
one rule that makes typing and jumping share a keyboard without a mode switch
between them. Every character that could extend *any* match on screen is
excluded, in both cases, so the answer does not depend on which match you were
looking at.

The label is drawn **after** its match, over the character that follows it, and
pressing it goes to the **start** of the match — you aim at the word and land
on its first character.

## What it searches

**The viewport of the focused window**, and nothing else. This is a jump, not
a search: something you cannot see is not somewhere you are aiming, and a
label on it could not be drawn anyway. `/` still exists for the file.

**Smartcase**, the same rule `/` follows: lowercase matches either case, and a
capital means you meant it. One search behaviour in the editor rather than two.

Matches do not overlap, so `aa` in `aaaa` is two matches and not three.

## `s` was `cl`

Vim's `s` is "substitute one character", which is `cl` spelled shorter, and
`cl` still works. Nothing else was given up: `S` stays `cc` until the
tree-sitter selection lands on it, and visual `s` is still change.

## How it is drawn

Three kinds of decoration, in this order, which is why the order a provider
pushes them in is the order they paint in:

1. one `Repaint` over the whole visible range in the theme's `dim`, so the
   syntax colours stop competing with the matches
2. one `Repaint` per match in `search` — the same colour `/` uses, because it
   is the same thing
3. one `Overlay` per label in `label`, on the `Over` layer

Nothing new reaches the frontend; the letters and the dimming arrive in the
same list as the indent guides.

## Tests

- Typing narrows: `f`, then `fu`, then `fun` each match fewer things.
- Every match gets a label, and no label is a character that could extend a
  match.
- Pressing a label puts the cursor on the first character of that match.
- A two-character label takes two presses.
- `Esc` leaves the cursor where it was; so does a query that matches nothing,
  which also leaves the mode.
- Backspace narrows back; on an empty query it leaves.
- Only the viewport is searched — a match below the fold gets no label.
- Smartcase: `fn` finds `Fn`, `Fn` does not find `fn`.
