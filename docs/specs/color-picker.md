# Colour picker

A colour lives in text as six or eight hex digits, and the decoration
(`colors.md`) paints them so you can see which colour. Picking a
different one still means typing digits. `:set editor color` with the
cursor on `#fb4934` draws a picker beside the text, and the keys move
through colour space by rewriting the digits in place. The file stays the
file; the picture is a view of it. `Enter` on an `rgb` or `rgba` row of
the property view (`props.md`) opens the same picker over the value, and
the gradient editor (`gradient.md`) opens it over a stop's colour: the
picker is the one component every colour goes through.

## Status

**Built.**

## What it looks like

Three windows, the curve's arrangement: the **source**, the **picker**
split to its right, the **form** down the edge. Focus lands on the
picker.

```
┌──────────────────────────────────┐
│                                  │   the square: saturation left to right,
│            ◯                     │   value (or lightness) bottom to top,
│                                  │   painted at the current hue; the ring
│                                  │   is where the colour is
└──────────────────────────────────┘
 ▌ red · yellow · green · cyan · blue · magenta · red ▐   the hue strip, 0..360
 ▌ ░░░░░░▒▒▒▒▒▒▓▓▓▓▓▓██████████████████████████ ▐       the alpha strip, over a checkerboard — only for eight digits
 ████████████████████│█████████████████████████         before | now
```

The square is 512 pixels a side, the strips 512 by 32 under it, the
swatch 512 by 48 at the bottom with the colour the picker opened on to
the left of the bar and the colour now to the right, so a change can be
judged against what it replaces. The component the keys act on is drawn
with a bright border; `Tab` moves it.

## Modes

The square is **HSV** by default — saturation across, value up — which is
what every picker since Photoshop draws, because a designer thinks "this
hue, more washed out, darker" and those are its two axes. `m` switches
the vertical axis to **lightness**, the HSL square, for the times a
palette is specified that way. RGB is never a picture: three sliders in
the form and `:tool color rgb` are exact, and a square cannot show three
axes. No wheel — a wheel is the hue strip bent round, prettier and worse
under `h` and `l`.

## The keys

On the picker, read from the normal keymap the way the plot reads its
keys, plus the few the picture takes as its own:

```
Tab  Shift-Tab   the next, previous component: square, hue, alpha
h  l  ← →        square: saturation down, up by one step; strips: hue or alpha
j  k  ↓ ↑        square: value (lightness) down, up; strips: the same as h and l
0  $             the component to its start, its end: grey, full colour; red; clear, opaque
gg  G            value to full, to none
m                the square's axis: value or lightness
yh               yank the colour as hex: `#fb4934`, `#fb493480`
yf               as floats: `0.984, 0.286, 0.204`, and the alpha when the literal has one
yr               as bytes: `251, 73, 52`, likewise
Enter            the colour on the ex line: `:tool color hex #fb4934`
u  Ctrl-R        undo, redo — the source buffer's
Esc              back to where you came from, the picker stays
:                the ex line
```

A step is a fiftieth of each range — `0.02` of saturation, `7.2` degrees
of hue, `5` of the 255 alpha steps — and counts multiply, so `5l` is a
tenth. `:tool color step 0.1` makes them coarser. Every key that changes
the colour is one edit of the source buffer and one undo step of it.

The yanks go into the register ring as `y` does in text, so `p` in any
buffer pastes them; the three spellings are what a stylesheet, a shader
and a byte-oriented API want.

**Hue and saturation survive.** A grey has no hue, black no saturation,
and a colour read back from six digits cannot say what they were. The
picker keeps its hue and saturation beside the colour, so `j` down to
black and `k` back up returns to the same red. After an edit made by hand
in the source the colour is re-read and the kept hue is dropped only when
it no longer produces the colour the text has.

**Ex forms:**

```
:tool color hex #fb4934        the colour; alpha too when the literal has it
:tool color rgb 251 73 52      bytes, an alpha fourth
:tool color floats 0.98 0.29 0.2
:tool color hsv 6 71 98        degrees, percent, percent
:tool color hsl 6 95 59
:tool color alpha 128          a byte
:tool color step 0.02
:tool color mode hsv|hsl
:tool color focus square|hue|alpha
:tool color yank hex|floats|rgb
```

`:tool color` alone lists these; each without a value reports. A value
that does not parse is refused naming what it takes.

**`:w` and `:q` are the code's**, as for the curve: `:w` from the picker
or the form writes the source buffer, `:q` closes both and puts the
cursor back. Closing any one of the three windows by hand closes the
tool's other two, since a form with no picture, or a picture with no
form, is half a tool.

## Finding the literal

A hex colour is `#` and six or eight hex digits, with the cursor anywhere
on them, on the `#`, or just after the last digit — the position `f` and
`$` leave it in. Quotes around it belong to the language and are left
alone. Three-digit CSS shorthand is not picked: the picker writes six
digits and would have to rewrite the shape of the literal. A cursor on no
such thing says `no colour under the cursor`.

**Writing back** replaces the digits and nothing else. Their case is kept
— a file written `#FB4934` stays upper — and a six-digit literal stays
six: the alpha strip is not drawn and alpha is not written. Eight digits
keep their alpha.

**The anchor** is the byte of the `#`. After every change of the buffer
the tool re-reads the digits from there; when they are gone, it looks
again from the source's cursor, or by its path when it was opened from
the property view, and otherwise says `colour lost` and blanks the
picture until the next `:set editor color`.

## The model

`src/color_picker.rs`, with no editor in it:

```rust
pub type Rgba = [u8; 4];
pub fn parse_hex(text: &str, alpha: bool) -> Option<Rgba>;      // `#` optional; six digits, or eight when alpha
pub fn hex(c: Rgba, alpha: bool) -> String;                       // lowercase, `#` first
pub fn floats(c: Rgba, alpha: bool) -> String;                    // `0.984, 0.286, 0.204`
pub fn bytes(c: Rgba, alpha: bool) -> String;                     // `251, 73, 52`

pub fn hsv_of(c: Rgba) -> (f32, f32, f32);                        // degrees, 0..1, 0..1
pub fn rgb_of_hsv(h: f32, s: f32, v: f32) -> [u8; 3];
pub fn hsl_of(c: Rgba) -> (f32, f32, f32);
pub fn rgb_of_hsl(h: f32, s: f32, l: f32) -> [u8; 3];

pub enum Mode { Hsv, Hsl }
pub enum Component { Square, Hue, Alpha }
/// Hue, saturation and value (or lightness) held beside the colour, so
/// a grey remembers what it was.
pub struct State { pub mode, pub hue, pub sat, pub val, pub alpha: u8 }
impl State {
    pub fn of(c: Rgba, mode: Mode) -> State;
    pub fn color(&self) -> Rgba;
    pub fn keeps(&self, c: Rgba) -> bool;          // whether this state still spells `c`
}

pub struct Literal { pub start: usize, pub end: usize, pub alpha: bool, pub upper: bool }
pub fn find(text: &str, cursor: usize) -> Option<Literal>;
pub fn write(lit: &Literal, c: Rgba) -> String;                   // the digits, as the literal spells them

pub fn render(state: &State, focus: Component, before: Rgba, alpha: bool) -> (u32, u32, Vec<u8>);
```

The picture is drawn on the same canvas the curve uses (`src/canvas.rs`,
lifted out of `curve.rs`), which gains a checkerboard and a filled
rectangle.

## The tool in the editor

`Tool::Color(ColorTool)` beside the curve: source, picture, form,
buffer, anchor, an optional path for the property view, the `State`, the
component in focus, the step, the colour it opened on, and a `back`
window — where `Esc` and `:q` put the focus, which is the source unless
the gradient editor opened the picker, in which case it is the gradient's
bar. The sync after every key re-reads the literal when the buffer's
edit counter or the form's generation moved, exactly as the curve's does.

The form:

```
R          ━━━━━━━●━━   251
G          ━━●━━━━━━━   73
B          ━●━━━━━━━━   52
A          ━━━━━━━━━●   255          only for eight digits
H          ━●━━━━━━━━   6
S          ━━━━━━━●━━   71
V          ━━━━━━━━━●   98           L in hsl mode
Mode       ‹ hsv ›
Step       ━●━━━━━━━━   0.020
```

Every field turns. A moved `R`, `G`, `B` or `A` sets the colour from the
bytes; a moved `H`, `S`, `V` or `L` sets the state; the picture and the
digits follow either way. `Enter` on a field prefills its `:tool color`
form.

The status row says `#fb4934  h 6 s 71 v 98` and `COLOR`.

## Tests

- `find` takes `#fb4934` with the cursor on the `#`, in the digits, or
  just after; takes eight digits as alpha; keeps the case; refuses three
  digits and a cursor elsewhere; leaves the quotes alone.
- `hsv_of` and `rgb_of_hsv` round-trip the corners and a mid colour;
  `hsl` the same; a grey keeps its hue through `State`.
- `hex`, `floats`, `bytes` spell a colour with and without alpha.
- `render` is 512 wide; the ring sits where the state says; a six-digit
  literal has no alpha strip.
- `:set editor color` on a cursor off a colour says so; on `#fb4934` it
  opens the picker right of the source and the form at the edge and
  focuses the picker; `l` rewrites the digits in place and nothing else
  changes; `5l` five steps; `Tab` to the hue strip and `l` turns the hue;
  `m` relabels the form; `yh`, `yf`, `yr` land in the register ring;
  `Enter` prefills `:tool color hex #…`; `u` restores; `:q` closes three
  windows; closing the form by hand closes the picker too; an upper-case
  literal stays upper; a hand edit of the digits redraws.
- `:tool color rgb 0 0 255` writes `#0000ff`; `hsv 120 100 100` writes
  `#00ff00`; `alpha 128` on a six-digit literal is refused; `mode hsl`
  relabels; `step 0.5` then `l` jumps.
- From the property view: `Enter` on `tint` opens the picker with the
  view as the source, `l` rewrites the JSON and the brick follows; an
  inherited colour is written first.
