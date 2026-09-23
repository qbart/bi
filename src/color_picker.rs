//! The colour picker's pure half: a hex literal in text, found under the
//! cursor and rewritten digit for digit; the colour as hue, saturation
//! and value or lightness, kept beside the bytes so a grey remembers its
//! hue; the three spellings a yank takes; and the picture — a square, a
//! hue strip, an alpha strip, a swatch — drawn into RGBA. No editor in
//! here. See `docs/specs/color-picker.md`.

use crate::canvas::Canvas;

pub type Rgba = [u8; 4];

// ---- hex -------------------------------------------------------------------

/// A colour read from its hex text: `#` optional, six digits, or eight
/// when `alpha`; six digits with `alpha` read as opaque. Case is free.
pub fn parse_hex(text: &str, alpha: bool) -> Option<Rgba> {
    let hex = text.trim().strip_prefix('#').unwrap_or(text.trim());
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    match (hex.len(), alpha) {
        (6, _) => Some([byte(0)?, byte(2)?, byte(4)?, 0xff]),
        (8, true) => Some([byte(0)?, byte(2)?, byte(4)?, byte(6)?]),
        _ => None,
    }
}

/// The colour as the editor writes it: lowercase, `#` first, eight
/// digits when `alpha`.
pub fn hex(c: Rgba, alpha: bool) -> String {
    if alpha {
        format!("#{:02x}{:02x}{:02x}{:02x}", c[0], c[1], c[2], c[3])
    } else {
        format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
    }
}

/// `0.984, 0.286, 0.204` — and the alpha fourth when asked.
pub fn floats(c: Rgba, alpha: bool) -> String {
    let n = if alpha { 4 } else { 3 };
    let parts: Vec<String> = c[..n].iter().map(|&b| format!("{:.3}", b as f32 / 255.0)).collect();
    parts.join(", ")
}

/// `251, 73, 52` — and the alpha fourth when asked.
pub fn bytes(c: Rgba, alpha: bool) -> String {
    let n = if alpha { 4 } else { 3 };
    let parts: Vec<String> = c[..n].iter().map(|b| b.to_string()).collect();
    parts.join(", ")
}

/// How a yank spells the colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Hex,
    Floats,
    Bytes,
}

impl Format {
    pub fn parse(text: &str) -> Option<Format> {
        Some(match text {
            "hex" | "h" => Format::Hex,
            "floats" | "float" | "f" => Format::Floats,
            "rgb" | "bytes" | "r" => Format::Bytes,
            _ => return None,
        })
    }

    pub fn spell(self, c: Rgba, alpha: bool) -> String {
        match self {
            Format::Hex => hex(c, alpha),
            Format::Floats => floats(c, alpha),
            Format::Bytes => bytes(c, alpha),
        }
    }
}

// ---- colour space ------------------------------------------------------------

/// Hue in degrees, saturation and value in `0..1`. A grey's hue is 0.
pub fn hsv_of(c: Rgba) -> (f32, f32, f32) {
    let (r, g, b) = (c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = hue_of(r, g, b, max, d);
    let s = if max <= 0.0 { 0.0 } else { d / max };
    (h, s, max)
}

fn hue_of(r: f32, g: f32, b: f32, max: f32, d: f32) -> f32 {
    if d <= 0.0 {
        return 0.0;
    }
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0).rem_euclid(360.0)
}

pub fn rgb_of_hsv(h: f32, s: f32, v: f32) -> [u8; 3] {
    let h = h.rem_euclid(360.0) / 60.0;
    let (s, v) = (s.clamp(0.0, 1.0), v.clamp(0.0, 1.0));
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [byte(r + m), byte(g + m), byte(b + m)]
}

/// Hue in degrees, saturation and lightness in `0..1`.
pub fn hsl_of(c: Rgba) -> (f32, f32, f32) {
    let (r, g, b) = (c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let l = (max + min) / 2.0;
    let h = hue_of(r, g, b, max, d);
    let s = if d <= 0.0 { 0.0 } else { d / (1.0 - (2.0 * l - 1.0).abs()).max(1e-6) };
    (h, s.clamp(0.0, 1.0), l)
}

pub fn rgb_of_hsl(h: f32, s: f32, l: f32) -> [u8; 3] {
    let h = h.rem_euclid(360.0) / 60.0;
    let (s, l) = (s.clamp(0.0, 1.0), l.clamp(0.0, 1.0));
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [byte(r + m), byte(g + m), byte(b + m)]
}

fn byte(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Which square the picker draws: value up, or lightness up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Hsv,
    Hsl,
}

impl Mode {
    pub fn parse(text: &str) -> Option<Mode> {
        match text {
            "hsv" => Some(Mode::Hsv),
            "hsl" => Some(Mode::Hsl),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Mode::Hsv => "hsv",
            Mode::Hsl => "hsl",
        }
    }

    /// The vertical axis's letter: `v` or `l`.
    pub fn axis(self) -> &'static str {
        match self {
            Mode::Hsv => "v",
            Mode::Hsl => "l",
        }
    }
}

/// What the keys act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Component {
    #[default]
    Square,
    Hue,
    Alpha,
}

impl Component {
    pub fn parse(text: &str) -> Option<Component> {
        match text {
            "square" => Some(Component::Square),
            "hue" => Some(Component::Hue),
            "alpha" => Some(Component::Alpha),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Component::Square => "square",
            Component::Hue => "hue",
            Component::Alpha => "alpha",
        }
    }

    /// The next component, wrapping; `alpha` skipped when the literal
    /// has none.
    pub fn next(self, back: bool, alpha: bool) -> Component {
        let ring: &[Component] = if alpha {
            &[Component::Square, Component::Hue, Component::Alpha]
        } else {
            &[Component::Square, Component::Hue]
        };
        let at = ring.iter().position(|&c| c == self).unwrap_or(0);
        let n = ring.len();
        ring[if back { (at + n - 1) % n } else { (at + 1) % n }]
    }
}

/// Hue, saturation and value (or lightness) held beside the colour, so
/// a grey remembers what it was. `hue` in degrees, the others `0..1`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct State {
    pub mode: Mode,
    pub hue: f32,
    pub sat: f32,
    pub val: f32,
    pub alpha: u8,
}

impl State {
    pub fn of(c: Rgba, mode: Mode) -> State {
        let (hue, sat, val) = match mode {
            Mode::Hsv => hsv_of(c),
            Mode::Hsl => hsl_of(c),
        };
        State { mode, hue, sat, val, alpha: c[3] }
    }

    /// The same colour, described in the other mode.
    pub fn in_mode(&self, mode: Mode) -> State {
        if mode == self.mode {
            return *self;
        }
        let mut next = State::of(self.color(), mode);
        // A grey or a black has no hue of its own: keep the one held.
        if next.sat <= 0.0 || next.val <= 0.0 || next.val >= 1.0 {
            next.hue = self.hue;
        }
        next
    }

    pub fn color(&self) -> Rgba {
        let [r, g, b] = match self.mode {
            Mode::Hsv => rgb_of_hsv(self.hue, self.sat, self.val),
            Mode::Hsl => rgb_of_hsl(self.hue, self.sat, self.val),
        };
        [r, g, b, self.alpha]
    }

    /// Whether this state still spells `c` — when it does not, the text
    /// was changed by another hand and the state is rebuilt from it.
    pub fn keeps(&self, c: Rgba) -> bool {
        self.color() == c
    }

    pub fn clamp(&mut self) {
        self.hue = self.hue.rem_euclid(360.0);
        self.sat = self.sat.clamp(0.0, 1.0);
        self.val = self.val.clamp(0.0, 1.0);
    }
}

// ---- the literal ------------------------------------------------------------

/// Where a hex colour sits in the text: `start` is the `#`, `end` one
/// past the last digit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Literal {
    pub start: usize,
    pub end: usize,
    pub alpha: bool,
    /// Any digit was upper case: written back that way.
    pub upper: bool,
}

impl Literal {
    pub fn digits(&self) -> usize {
        if self.alpha { 8 } else { 6 }
    }
}

/// The hex colour at byte `cursor`: the cursor on the `#`, on a digit,
/// or just after the last one. Six or eight digits, not three.
pub fn find(text: &str, cursor: usize) -> Option<Literal> {
    let bytes = text.as_bytes();
    let cursor = cursor.min(bytes.len());
    // Walk back over hex digits to a `#`, at most nine bytes.
    let mut at = cursor;
    let mut back = 0;
    while at > 0 && back < 9 && bytes[at - 1].is_ascii_hexdigit() {
        at -= 1;
        back += 1;
    }
    let start = if bytes.get(at) == Some(&b'#') {
        at
    } else if at > 0 && bytes[at - 1] == b'#' {
        at - 1
    } else {
        return None;
    };
    at_hash(text, start).filter(|lit| cursor <= lit.end)
}

/// The literal whose `#` is at `start`, when six or eight digits follow.
pub fn at_hash(text: &str, start: usize) -> Option<Literal> {
    let bytes = text.as_bytes();
    if bytes.get(start) != Some(&b'#') {
        return None;
    }
    let mut end = start + 1;
    while end < bytes.len() && bytes[end].is_ascii_hexdigit() {
        end += 1;
    }
    let n = end - start - 1;
    if n != 6 && n != 8 {
        return None;
    }
    let upper = text[start + 1..end].bytes().any(|b| b.is_ascii_uppercase());
    Some(Literal { start, end, alpha: n == 8, upper })
}

/// The literal's new text for colour `c`: the digits as the literal
/// spells them, `#` included.
pub fn write(lit: &Literal, c: Rgba) -> String {
    let out = hex(c, lit.alpha);
    if lit.upper { out.to_ascii_uppercase() } else { out }
}

// ---- the picture ---------------------------------------------------------------

pub const SIDE: i64 = 512;
const STRIP: i64 = 32;
const SWATCH: i64 = 48;
const GAP: i64 = 8;
const RING: [u8; 4] = [255, 255, 255, 255];
const RING_DARK: [u8; 4] = [0, 0, 0, 255];
const FOCUS: [u8; 4] = [255, 210, 130, 255];

/// Where each part sits, top to bottom.
struct Layout {
    square: i64,
    hue: i64,
    alpha: Option<i64>,
    swatch: i64,
    height: i64,
}

fn layout(alpha: bool) -> Layout {
    let square = GAP;
    let hue = square + SIDE + GAP;
    let (alpha_at, next) = if alpha {
        (Some(hue + STRIP + GAP), hue + 2 * (STRIP + GAP))
    } else {
        (None, hue + STRIP + GAP)
    };
    Layout { square, hue, alpha: alpha_at, swatch: next, height: next + SWATCH + GAP }
}

/// The picker: the square at the current hue, the hue strip, the alpha
/// strip when the literal has one, the swatch with the colour it opened
/// on and the colour now, and the focused part outlined. Returns
/// `(width, height, rgba)`.
pub fn render(state: &State, focus: Component, before: Rgba, alpha: bool) -> (u32, u32, Vec<u8>) {
    let l = layout(alpha);
    let w = SIDE + 2 * GAP;
    let mut cv = Canvas::new(w as u32, l.height as u32);
    let x0 = GAP;

    // The square: saturation across, value or lightness up.
    for row in 0..SIDE {
        let v = 1.0 - row as f32 / (SIDE - 1) as f32;
        for col in 0..SIDE {
            let s = col as f32 / (SIDE - 1) as f32;
            let [r, g, b] = match state.mode {
                Mode::Hsv => rgb_of_hsv(state.hue, s, v),
                Mode::Hsl => rgb_of_hsl(state.hue, s, v),
            };
            cv.put(x0 + col, l.square + row, [r, g, b, 255]);
        }
    }
    let ring_x = x0 + (state.sat * (SIDE - 1) as f32).round() as i64;
    let ring_y = l.square + ((1.0 - state.val) * (SIDE - 1) as f32).round() as i64;
    ring(&mut cv, ring_x, ring_y, 7);

    // The hue strip.
    for col in 0..SIDE {
        let h = col as f32 / (SIDE - 1) as f32 * 360.0;
        let [r, g, b] = rgb_of_hsv(h, 1.0, 1.0);
        cv.line(x0 + col, l.hue, x0 + col, l.hue + STRIP - 1, [r, g, b, 255]);
    }
    let hue_x = x0 + (state.hue.rem_euclid(360.0) / 360.0 * (SIDE - 1) as f32).round() as i64;
    marker(&mut cv, hue_x, l.hue, STRIP);

    // The alpha strip: the colour, clear to opaque, over a checkerboard.
    if let Some(top) = l.alpha {
        cv.checker(x0, top, SIDE, STRIP, 8);
        let c = state.color();
        for col in 0..SIDE {
            let a = (col as f32 / (SIDE - 1) as f32 * 255.0).round() as u8;
            for row in 0..STRIP {
                cv.blend(x0 + col, top + row, [c[0], c[1], c[2], a]);
            }
        }
        let alpha_x = x0 + (state.alpha as f32 / 255.0 * (SIDE - 1) as f32).round() as i64;
        marker(&mut cv, alpha_x, top, STRIP);
    }

    // The swatch: before on the left, now on the right.
    cv.checker(x0, l.swatch, SIDE, SWATCH, 8);
    let now = state.color();
    for row in 0..SWATCH {
        for col in 0..SIDE {
            let c = if col < SIDE / 2 { before } else { now };
            let c = if alpha { c } else { [c[0], c[1], c[2], 255] };
            cv.blend(x0 + col, l.swatch + row, c);
        }
    }
    cv.line(x0 + SIDE / 2, l.swatch, x0 + SIDE / 2, l.swatch + SWATCH - 1, RING_DARK);

    // The focused part, outlined.
    let (fx, fy, fw, fh) = match focus {
        Component::Square => (x0, l.square, SIDE, SIDE),
        Component::Hue => (x0, l.hue, SIDE, STRIP),
        Component::Alpha => (x0, l.alpha.unwrap_or(l.hue), SIDE, STRIP),
    };
    cv.frame(fx - 2, fy - 2, fw + 4, fh + 4, 2, FOCUS);

    let (w, h) = cv.size();
    (w, h, cv.into_pixels())
}

/// A ring, white with a dark edge, so it shows on any colour.
fn ring(cv: &mut Canvas, cx: i64, cy: i64, r: i64) {
    for (radius, colour) in [(r + 1, RING_DARK), (r, RING), (r - 1, RING_DARK)] {
        let mut a = 0.0f32;
        while a < std::f32::consts::TAU {
            let (x, y) = (
                cx + (radius as f32 * a.cos()).round() as i64,
                cy + (radius as f32 * a.sin()).round() as i64,
            );
            cv.put(x, y, colour);
            a += 0.02;
        }
    }
}

/// A strip's marker: a light line with dark edges.
fn marker(cv: &mut Canvas, x: i64, top: i64, height: i64) {
    cv.line(x - 2, top, x - 2, top + height - 1, RING_DARK);
    cv.line(x + 2, top, x + 2, top + height - 1, RING_DARK);
    cv.line(x - 1, top, x - 1, top + height - 1, RING);
    cv.line(x + 1, top, x + 1, top + height - 1, RING);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_reads_and_writes() {
        assert_eq!(parse_hex("#c83c1e", false), Some([0xc8, 0x3c, 0x1e, 0xff]));
        assert_eq!(parse_hex("C83C1E", false), Some([0xc8, 0x3c, 0x1e, 0xff]));
        assert_eq!(parse_hex("#c83c1e80", true), Some([0xc8, 0x3c, 0x1e, 0x80]));
        assert_eq!(parse_hex("#c83c1e", true), Some([0xc8, 0x3c, 0x1e, 0xff]), "six read opaque");
        assert_eq!(parse_hex("#c83c1e80", false), None, "no alpha on an rgb");
        assert_eq!(parse_hex("red", false), None);
        assert_eq!(parse_hex("#c83c1", false), None);
        assert_eq!(hex([0xc8, 0x3c, 0x1e, 0x80], false), "#c83c1e");
        assert_eq!(hex([0xc8, 0x3c, 0x1e, 0x80], true), "#c83c1e80");
        assert_eq!(floats([251, 73, 52, 128], false), "0.984, 0.286, 0.204");
        assert_eq!(floats([251, 73, 52, 128], true), "0.984, 0.286, 0.204, 0.502");
        assert_eq!(bytes([251, 73, 52, 128], false), "251, 73, 52");
        assert_eq!(bytes([251, 73, 52, 128], true), "251, 73, 52, 128");
        assert_eq!(Format::parse("floats"), Some(Format::Floats));
        assert_eq!(Format::Hex.spell([1, 2, 3, 4], true), "#01020304");
    }

    #[test]
    fn hsv_and_hsl_round_trip_and_a_grey_keeps_its_hue() {
        for c in [
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [251, 73, 52, 255],
            [17, 200, 99, 255],
            [0, 0, 0, 255],
            [255, 255, 255, 255],
            [128, 128, 128, 255],
        ] {
            let (h, s, v) = hsv_of(c);
            assert_eq!(rgb_of_hsv(h, s, v), [c[0], c[1], c[2]], "hsv {c:?}");
            let (h, s, l) = hsl_of(c);
            assert_eq!(rgb_of_hsl(h, s, l), [c[0], c[1], c[2]], "hsl {c:?}");
        }
        assert_eq!(hsv_of([255, 0, 0, 255]).0, 0.0);
        assert_eq!(hsv_of([0, 255, 0, 255]).0, 120.0);
        assert_eq!(hsv_of([0, 0, 255, 255]).0, 240.0);
        assert!((hsl_of([255, 0, 0, 255]).2 - 0.5).abs() < 1e-6);

        let mut red = State::of([255, 0, 0, 255], Mode::Hsv);
        red.val = 0.0;
        assert_eq!(red.color(), [0, 0, 0, 255]);
        assert!(red.keeps([0, 0, 0, 255]));
        assert!(!red.keeps([1, 0, 0, 255]));
        red.val = 1.0;
        assert_eq!(red.color(), [255, 0, 0, 255], "the hue survived black");
        let grey = State::of([128, 128, 128, 255], Mode::Hsv);
        assert_eq!(
            State { hue: 200.0, ..grey }.in_mode(Mode::Hsl).hue,
            200.0,
            "held through a mode change"
        );
        assert_eq!(Component::Square.next(false, false), Component::Hue);
        assert_eq!(Component::Hue.next(false, false), Component::Square, "no alpha: two parts");
        assert_eq!(Component::Hue.next(false, true), Component::Alpha);
        assert_eq!(Component::Square.next(true, true), Component::Alpha);
    }

    #[test]
    fn find_takes_six_or_eight_digits_around_the_cursor_and_keeps_the_case() {
        let text = r##"color: "#FB4934"; other = #00ff0080; short #abc;"##;
        let at = text.find("#FB").unwrap();
        let lit = Literal { start: at, end: at + 7, alpha: false, upper: true };
        assert_eq!(find(text, at), Some(lit.clone()), "on the hash");
        assert_eq!(find(text, at + 3), Some(lit.clone()), "in the digits");
        assert_eq!(find(text, at + 7), Some(lit.clone()), "just after");
        assert_eq!(find(text, at + 8), None, "on the quote after");
        assert_eq!(find(text, 0), None);
        let at2 = text.find("#00ff").unwrap();
        assert_eq!(
            find(text, at2 + 4),
            Some(Literal { start: at2, end: at2 + 9, alpha: true, upper: false })
        );
        assert_eq!(find(text, text.find("#abc").unwrap() + 1), None, "three digits are not picked");
        assert_eq!(write(&lit, [0xc8, 0x3c, 0x1e, 0x80]), "#C83C1E");
        assert_eq!(
            write(&Literal { alpha: true, upper: false, ..lit }, [0xc8, 0x3c, 0x1e, 0x80]),
            "#c83c1e80"
        );
    }

    #[test]
    fn render_is_the_square_the_strips_and_the_swatch() {
        let state = State::of([255, 0, 0, 255], Mode::Hsv);
        let (w, h, px) = render(&state, Component::Square, [0, 0, 255, 255], false);
        assert_eq!(w as i64, SIDE + 2 * GAP);
        assert_eq!(h as i64, GAP + SIDE + GAP + STRIP + GAP + SWATCH + GAP, "no alpha strip");
        let (_, h2, _) = render(&state, Component::Alpha, [0, 0, 255, 255], true);
        assert_eq!(h2 as i64, h as i64 + STRIP + GAP, "the alpha strip");
        // The square's top-right is the full hue, its bottom-left black.
        let at = |x: i64, y: i64| {
            let i = ((y as u32 * w + x as u32) * 4) as usize;
            [px[i], px[i + 1], px[i + 2]]
        };
        assert_eq!(at(GAP + SIDE - 1, GAP), [255, 0, 0]);
        assert_eq!(at(GAP, GAP + SIDE - 1), [0, 0, 0]);
        assert_eq!(at(GAP, GAP), [255, 255, 255]);
        // The swatch: before on the left, now on the right.
        let swatch = GAP + SIDE + GAP + STRIP + GAP + SWATCH / 2;
        assert_eq!(at(GAP + 10, swatch), [0, 0, 255]);
        assert_eq!(at(GAP + SIDE - 10, swatch), [255, 0, 0]);
    }
}
