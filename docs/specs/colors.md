# Colours, in the colour they name

`#fb4934` is six characters that mean a colour, and no amount of staring at
them tells you which one. Painting the text in the colour it names is the whole
feature, and it is one more decoration provider.

## Status

**Built.**

## What is recognised

```
#fb4934          six hex digits
#f94             three, each doubled — CSS's shorthand
#fb4934ff        eight, the last two an alpha bi ignores
rgb(251,73,52)   integers, 0–255, spaces allowed after the commas
rgba(251,73,52,0.5)
rgb(0.5f,0.1f,0.1)   floats, where 1.0 is 255 — the shader spelling
```

**Per component**, a number that contains a `.` or ends in `f` is a float and
is scaled by 255; anything else is an integer taken as it stands. That is what
lets `rgb(0.5f, 0.1f, 0.1)` work, and it means a file that mixes the two
spellings gets both right rather than picking one and being wrong half the
time.

Out of range clamps rather than being rejected: `rgb(300,0,0)` is red, which is
what the person who typed it meant and what every renderer they will hand it to
does.

**Alpha is parsed and ignored.** Blending it against the background would need
a background to blend against, and the text is sitting on whatever the theme
and the cursorline and the selection have already put there. A swatch says
"this is the colour", not "this is what it will look like over your widget".

Hex is matched with the longest form first — eight digits, then six, then three
— and the run has to *end* there: `#fb4934ff` is one eight-digit colour, never a
six-digit one with `ff` after it.

## The text colour

The swatch paints the background; the foreground has to stay readable on it, so
it is black or white and nothing else, chosen by relative luminance:

```
L = 0.2126·R + 0.7152·G + 0.0722·B      on linearised channels
white when L < 0.179, else black
```

That is the WCAG contrast rule rather than a brightness average, and it is
worth the ten lines: an average puts white text on saturated green, which is
the one case a naive formula always gets wrong and the one people notice.

## Where it applies

Everywhere in the file, like the `TODO:` markers and for the same reason:
colours turn up in CSS, in shaders, in JSON themes, in Markdown, in
configuration, and in comments describing all four. A rule that needed a
grammar would go quiet in exactly the files people keep palettes in.

```
[options]
color_swatches = true
```

## How it works

A `Repaint` decoration per match (`docs/specs/decorations.md`), over the char
range of the literal, on the `Under` layer. The style has an exact `Rgb`
background and a black or white foreground — a colour no theme can name, which
is why decorations carry a resolved style rather than a theme key.

## Tests

- Each spelling parses to the same colour: `#f94`, `#ff9944`, `rgb(255,153,68)`.
- A float component scales by 255, an integer one does not, and a line may mix
  them.
- Eight hex digits are one match, not a six and a leftover.
- Out of range clamps.
- Black text on a light swatch, white on a dark one, and the green that a
  brightness average gets wrong gets black.
- The range covers the literal and nothing around it.
- Off produces nothing at all.
