//! Colour literals, found in a line.
//!
//! `#fb4934` is six characters that mean a colour and no amount of staring at
//! them says which. This is the scan that finds them and the arithmetic that
//! decides whether the text over one should be black or white; the drawing is
//! a decoration.
//!
//! See `docs/specs/colors.md`.

use std::ops::Range;

/// A colour a line named, and where it named it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Swatch {
    pub range: Range<usize>,
    pub rgb: (u8, u8, u8),
}

/// Every colour literal in `line`, as char ranges within it.
pub fn swatches(line: &str) -> Vec<Swatch> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        let found = match chars[i] {
            '#' => hex(&chars, i),
            'r' | 'R' => functional(&chars, i),
            _ => None,
        };
        match found {
            Some(swatch) => {
                i = swatch.range.end;
                out.push(swatch);
            }
            None => i += 1,
        }
    }
    out
}

/// `#rgb`, `#rrggbb`, `#rrggbbaa`.
///
/// Longest first, and the run has to end where the form does: `#fb4934ff` is
/// one eight-digit colour, never a six-digit one with `ff` after it.
fn hex(chars: &[char], at: usize) -> Option<Swatch> {
    let digits = chars[at + 1..].iter().take_while(|c| c.is_ascii_hexdigit()).count();
    let len = match digits {
        8 => 8,
        6 => 6,
        3 => 3,
        _ => return None,
    };
    let value = |i: usize| chars[at + 1 + i].to_digit(16).unwrap_or(0) as u8;
    let rgb = match len {
        3 => (value(0) * 17, value(1) * 17, value(2) * 17),
        // The last two digits of the eight-digit form are alpha, and alpha is
        // parsed and ignored: blending needs a background to blend against,
        // and the text is already sitting on the theme's.
        _ => (value(0) * 16 + value(1), value(2) * 16 + value(3), value(4) * 16 + value(5)),
    };
    Some(Swatch { range: at..at + 1 + len, rgb })
}

/// `rgb(...)` and `rgba(...)`, in integers or in floats.
fn functional(chars: &[char], at: usize) -> Option<Swatch> {
    let word: String = chars[at..].iter().take(5).collect::<String>().to_lowercase();
    let open = if word.starts_with("rgba(") {
        at + 5
    } else if word.starts_with("rgb(") {
        at + 4
    } else {
        return None;
    };

    let close = at + chars[at..].iter().position(|&c| c == ')')?;
    let inside: String = chars[open..close].iter().collect();
    let parts: Vec<&str> = inside.split(',').map(str::trim).collect();
    if parts.len() < 3 || parts.len() > 4 {
        return None;
    }

    // One decision for the whole literal, taken from the three colour
    // components: a float anywhere among them says which of the two spellings
    // this is, and the bare `1`s beside it are the same 1.0. Per component,
    // `rgb(1,1,1.0f)` reads as two channels of almost nothing and one of
    // everything, and paints white blue.
    let floats = parts[..3].iter().any(|part| is_float(part));
    let mut rgb = [0u8; 3];
    for (slot, part) in rgb.iter_mut().zip(&parts) {
        *slot = channel(part, floats)?;
    }
    // The alpha, if there is one, has to at least be a number — `rgba(1,2,3,x)`
    // is not a colour and should not be painted as one. It has no say in the
    // decision above and takes none from it: alpha is 0 to 1 in both
    // spellings, which is why `rgba(255,153,68,0.5)` is still an integer
    // colour.
    if parts.len() == 4 && !is_number(parts[3]) {
        return None;
    }
    Some(Swatch { range: at..close + 1, rgb: (rgb[0], rgb[1], rgb[2]) })
}

/// Whether a component is written as a float: a `.` in it, or an `f` after it.
fn is_float(text: &str) -> bool {
    let text = text.trim();
    text.contains('.') || text.strip_suffix(['f', 'F']).is_some_and(|bare| !bare.is_empty())
}

/// Whether a component is a number at all, in either spelling.
fn is_number(text: &str) -> bool {
    let text = text.trim();
    let bare = text.strip_suffix(['f', 'F']).unwrap_or(text);
    !bare.is_empty() && bare.parse::<f64>().is_ok()
}

/// One component, read in the space the literal is written in: a float where
/// 1.0 is 255, or an integer taken as it stands.
///
/// Out of range clamps: `rgb(300,0,0)` is red, which is what the person who
/// typed it meant.
fn channel(text: &str, float: bool) -> Option<u8> {
    let text = text.trim();
    let bare = text.strip_suffix(['f', 'F']).unwrap_or(text);
    if bare.is_empty() {
        return None;
    }
    if float {
        let value: f64 = bare.parse().ok()?;
        return Some((value * 255.0).round().clamp(0.0, 255.0) as u8);
    }
    let value: i64 = bare.parse().ok()?;
    Some(value.clamp(0, 255) as u8)
}

/// Black or white, whichever can be read on `rgb`.
///
/// WCAG's relative luminance rather than a brightness average: an average puts
/// white on saturated green, which is the one case a naive formula always gets
/// wrong and the one people notice.
pub fn readable_on(rgb: (u8, u8, u8)) -> (u8, u8, u8) {
    let linear = |c: u8| {
        let c = c as f64 / 255.0;
        match c <= 0.04045 {
            true => c / 12.92,
            false => ((c + 0.055) / 1.055).powf(2.4),
        }
    };
    let luminance = 0.2126 * linear(rgb.0) + 0.7152 * linear(rgb.1) + 0.0722 * linear(rgb.2);
    match luminance < 0.179 {
        true => (255, 255, 255),
        false => (0, 0, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(line: &str) -> Vec<(String, (u8, u8, u8))> {
        swatches(line)
            .into_iter()
            .map(|s| (line.chars().skip(s.range.start).take(s.range.len()).collect(), s.rgb))
            .collect()
    }

    #[test]
    fn every_spelling_reaches_the_same_colour() {
        let orange = (0xff, 0x99, 0x44);
        assert_eq!(found("#f94").first().unwrap().1, orange);
        assert_eq!(found("#ff9944").first().unwrap().1, orange);
        assert_eq!(found("#FF9944").first().unwrap().1, orange);
        assert_eq!(found("rgb(255,153,68)").first().unwrap().1, orange);
        assert_eq!(found("rgba(255, 153, 68, 0.5)").first().unwrap().1, orange);
        assert_eq!(found("RGB(255,153,68)").first().unwrap().1, orange);
    }

    #[test]
    fn floats_scale_by_255_and_integers_do_not() {
        assert_eq!(found("rgb(1.0f,0.0,0)").first().unwrap().1, (255, 0, 0));
        assert_eq!(found("rgb(0.5f,0.1f,0.1)").first().unwrap().1, (128, 26, 26));
        assert_eq!(found("rgb(255,153,68)").first().unwrap().1, (0xff, 0x99, 0x44));
    }

    /// One float makes the whole literal a float: the bare `1`s beside it are
    /// the same 1.0, and reading them as integers paints white blue.
    #[test]
    fn one_float_component_puts_every_component_in_float_space() {
        let white = (255, 255, 255);
        assert_eq!(found("rgb(1,1,1.0f)").first().unwrap().1, white);
        assert_eq!(found("rgb(1.0,1.0,1.0)").first().unwrap().1, white);
        assert_eq!(found("rgb(1,1.0,1)").first().unwrap().1, white);
        assert_eq!(found("rgb(1,1,1)").first().unwrap().1, (1, 1, 1), "and no float, no scaling");
    }

    /// Alpha is 0 to 1 in both spellings, so it says nothing about which one
    /// the colour is written in.
    #[test]
    fn the_alpha_does_not_decide_the_space() {
        assert_eq!(found("rgba(255, 153, 68, 0.5)").first().unwrap().1, (0xff, 0x99, 0x44));
        assert_eq!(found("rgba(1.0, 0.6, 0.267, 0.5)").first().unwrap().1, (255, 153, 68));
    }

    #[test]
    fn eight_digits_are_one_colour_and_not_a_six_with_leftovers() {
        assert_eq!(found("#fb4934ff"), [("#fb4934ff".to_string(), (0xfb, 0x49, 0x34))]);
        assert_eq!(found("#fb4934"), [("#fb4934".to_string(), (0xfb, 0x49, 0x34))]);
        // Four, five and seven are no form at all.
        assert!(found("#fb49").is_empty());
        assert!(found("#fb4934f").is_empty());
    }

    #[test]
    fn out_of_range_clamps_rather_than_refusing() {
        assert_eq!(found("rgb(300,-5,0)").first().unwrap().1, (255, 0, 0));
    }

    #[test]
    fn what_is_not_a_colour_is_left_alone() {
        assert!(found("rgb(1,2,x)").is_empty());
        assert!(found("rgb(1,2)").is_empty());
        assert!(found("rgb(1,2,3,4,5)").is_empty());
        assert!(found("register(a, b, c)").is_empty());
        assert!(found("#nothex").is_empty());
    }

    #[test]
    fn the_range_covers_the_literal_and_nothing_around_it() {
        assert_eq!(found("bg: #fb4934; /* red */"), [("#fb4934".to_string(), (0xfb, 0x49, 0x34))]);
        assert_eq!(found("  rgb(1, 2, 3)  "), [("rgb(1, 2, 3)".to_string(), (1, 2, 3))]);
    }

    #[test]
    fn two_on_one_line_are_two() {
        assert_eq!(found("#000000 #ffffff").len(), 2);
    }

    #[test]
    fn the_text_over_a_swatch_is_whichever_of_black_and_white_can_be_read() {
        assert_eq!(readable_on((255, 255, 255)), (0, 0, 0));
        assert_eq!(readable_on((0, 0, 0)), (255, 255, 255));
        assert_eq!(readable_on((0x28, 0x28, 0x28)), (255, 255, 255), "gruvbox's background");
        // The case a brightness average gets wrong: saturated green is bright
        // enough to need black text and reads as mid-grey to an average.
        assert_eq!(readable_on((0, 255, 0)), (0, 0, 0));
    }
}
