//! The gradient editor's pure half: a list of stops in text — each a
//! position and a hex colour — found by a bracket scan, read, evaluated
//! as a ramp, rewritten stop by stop, and drawn into RGBA. No editor in
//! here. See `docs/specs/gradient.md`.

use crate::canvas::Canvas;
use crate::color_picker::{self, Rgba};
use crate::curve::{Token, number_at, rewrite, span_start};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stop {
    pub t: f32,
    pub color: Rgba,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Gradient {
    /// In the order the text has them; sorted by `t` by whoever edits.
    pub stops: Vec<Stop>,
}

/// Where one stop's tokens sit in the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopSpan {
    /// The opening bracket, or a name glued to it.
    pub start: usize,
    /// One past the closing bracket.
    pub end: usize,
    pub number: Token,
    pub color: color_picker::Literal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Literal {
    pub open: usize,
    pub close: usize,
    pub stops: Vec<StopSpan>,
}

// ---- finding the literal ----------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tok {
    Number(Token),
    Color(usize),
}

struct Group {
    open: usize,
    close: usize,
    tokens: Vec<Tok>,
    children: Vec<Group>,
}

impl Group {
    fn contains(&self, at: usize) -> bool {
        self.open <= at && at <= self.close
    }

    /// One number and one colour, directly inside, nothing nested.
    fn is_stop(&self) -> bool {
        self.children.is_empty()
            && self.tokens.len() == 2
            && self.tokens.iter().filter(|t| matches!(t, Tok::Number(_))).count() == 1
            && self.tokens.iter().filter(|t| matches!(t, Tok::Color(_))).count() == 1
    }

    fn is_list(&self) -> bool {
        !self.children.is_empty()
            && self.tokens.is_empty()
            && self.children.iter().all(Group::is_stop)
    }
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn parse_groups(text: &str) -> Group {
    let bytes = text.as_bytes();
    let mut stack =
        vec![Group { open: 0, close: bytes.len(), tokens: Vec::new(), children: Vec::new() }];
    let mut i = 0;
    let mut prev_ident = false;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'{' | b'[' | b'(' => {
                stack.push(Group { open: i, close: i, tokens: Vec::new(), children: Vec::new() });
                i += 1;
                prev_ident = false;
            }
            b'}' | b']' | b')' => {
                if stack.len() > 1 {
                    let mut group = stack.pop().unwrap();
                    group.close = i;
                    stack.last_mut().unwrap().children.push(group);
                }
                i += 1;
                prev_ident = true;
            }
            b'#' => match color_picker::at_hash(text, i) {
                Some(lit) => {
                    stack.last_mut().unwrap().tokens.push(Tok::Color(lit.start));
                    i = lit.end;
                    prev_ident = true;
                }
                None => {
                    i += 1;
                    prev_ident = false;
                }
            },
            _ if b.is_ascii_digit() || ((b == b'-' || b == b'+' || b == b'.') && !prev_ident) => {
                match number_at(bytes, i) {
                    Some(end) => {
                        stack.last_mut().unwrap().tokens.push(Tok::Number(Token { start: i, end }));
                        i = end;
                        prev_ident = true;
                    }
                    None => {
                        i += 1;
                        prev_ident = false;
                    }
                }
            }
            _ if is_ident(b) => {
                while i < bytes.len() && is_ident(bytes[i]) {
                    i += 1;
                }
                prev_ident = true;
            }
            _ => {
                i += 1;
                prev_ident = b == b')' || b == b']';
            }
        }
    }
    while stack.len() > 1 {
        let group = stack.pop().unwrap();
        stack.last_mut().unwrap().children.push(group);
    }
    stack.pop().unwrap()
}

/// The innermost list of stops around byte `cursor`.
pub fn find(text: &str, cursor: usize) -> Option<Literal> {
    let root = parse_groups(text);
    let mut best: Option<&Group> = None;
    let mut node = &root;
    loop {
        if !std::ptr::eq(node, &root) && node.is_list() {
            best = Some(node);
        }
        match node.children.iter().find(|c| c.contains(cursor)) {
            Some(child) => node = child,
            None => break,
        }
    }
    let group = best?;
    let stops = group
        .children
        .iter()
        .map(|c| {
            let number = c
                .tokens
                .iter()
                .find_map(|t| match t {
                    Tok::Number(n) => Some(*n),
                    _ => None,
                })
                .expect("a stop has a number");
            let at = c
                .tokens
                .iter()
                .find_map(|t| match t {
                    Tok::Color(at) => Some(*at),
                    _ => None,
                })
                .expect("a stop has a colour");
            StopSpan {
                start: span_start(text, c.open),
                end: c.close + 1,
                number,
                color: color_picker::at_hash(text, at).expect("scanned as a colour"),
            }
        })
        .collect();
    Some(Literal { open: group.open, close: group.close, stops })
}

pub fn read(text: &str, lit: &Literal) -> Gradient {
    let stops = lit
        .stops
        .iter()
        .map(|s| Stop {
            t: text[s.number.start..s.number.end]
                .trim_end_matches(|c: char| c.is_ascii_alphabetic() || c == '_')
                .parse()
                .unwrap_or(0.0),
            color: color_picker::parse_hex(&text[s.color.start..s.color.end], s.color.alpha)
                .unwrap_or([0, 0, 0, 255]),
        })
        .collect();
    Gradient { stops }
}

/// The black-to-white ramp: what an empty gradient is seeded with.
pub fn linear() -> Gradient {
    Gradient {
        stops: vec![
            Stop { t: 0.0, color: [0, 0, 0, 255] },
            Stop { t: 1.0, color: [255, 255, 255, 255] },
        ],
    }
}

/// The ramp's colour at `t`: linear in sRGB between the neighbouring
/// stops, flat outside them. Reads the stops in sorted order whatever
/// order they sit in.
pub fn eval(g: &Gradient, t: f32) -> Rgba {
    let order = sorted_order(g);
    let Some(&first) = order.first() else { return [0, 0, 0, 255] };
    let stops: Vec<Stop> = order.iter().map(|&i| g.stops[i]).collect();
    if t <= stops[0].t {
        return g.stops[first].color;
    }
    for pair in stops.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if t <= b.t {
            let d = b.t - a.t;
            let f = if d <= 0.0 { 1.0 } else { (t - a.t) / d };
            let mut out = [0u8; 4];
            for (i, slot) in out.iter_mut().enumerate() {
                *slot =
                    (a.color[i] as f32 + (b.color[i] as f32 - a.color[i] as f32) * f).round() as u8;
            }
            return out;
        }
    }
    stops.last().map_or([0, 0, 0, 255], |s| s.color)
}

/// The stops' indices sorted by `t`, stable: slot `k` of the sorted
/// list holds stop `order[k]`.
pub fn sorted_order(g: &Gradient) -> Vec<usize> {
    let mut order: Vec<usize> = (0..g.stops.len()).collect();
    order.sort_by(|&a, &b| {
        g.stops[a].t.partial_cmp(&g.stops[b].t).unwrap_or(std::cmp::Ordering::Equal)
    });
    order
}

/// A stop's text shaped like `like` with `t` and `color` written in:
/// the number by the curve's rule, the digits by the picker's.
pub fn stop_text(text: &str, like: &StopSpan, t: f32, color: Rgba, step: f32) -> String {
    let mut out = text[like.start..like.end].to_string();
    let base = like.start;
    // Right to left, so the earlier offsets hold.
    let mut edits = [
        (
            like.number.start,
            like.number.end,
            rewrite(&text[like.number.start..like.number.end], t, step),
        ),
        (like.color.start, like.color.end, color_picker::write(&like.color, color)),
    ];
    edits.sort_by_key(|e| std::cmp::Reverse(e.0));
    for (start, end, new) in edits {
        out.replace_range(start - base..end - base, &new);
    }
    out
}

// ---- the picture -----------------------------------------------------------------

pub const WIDTH: i64 = 512;
const BAR: i64 = 64;
const TRIANGLES: i64 = 24;
const GAP: i64 = 8;
const OUTLINE: [u8; 4] = [20, 20, 24, 255];
const PICK: [u8; 4] = [255, 255, 255, 255];

/// The ramp over a checkerboard, a triangle under each stop filled its
/// colour, the selected one white. Returns `(width, height, rgba)`.
pub fn render(g: &Gradient, selected: usize) -> (u32, u32, Vec<u8>) {
    let w = WIDTH + 2 * GAP;
    let h = GAP + BAR + TRIANGLES + GAP;
    let mut cv = Canvas::new(w as u32, h as u32);
    let (x0, top) = (GAP, GAP);
    cv.checker(x0, top, WIDTH, BAR, 8);
    for col in 0..WIDTH {
        let c = eval(g, col as f32 / (WIDTH - 1) as f32);
        for row in 0..BAR {
            cv.blend(x0 + col, top + row, c);
        }
    }
    cv.frame(x0 - 1, top - 1, WIDTH + 2, BAR + 2, 1, OUTLINE);
    let base = top + BAR + 2;
    for (i, stop) in g.stops.iter().enumerate() {
        let x = x0 + (stop.t.clamp(0.0, 1.0) * (WIDTH - 1) as f32).round() as i64;
        let fill =
            if i == selected { PICK } else { [stop.color[0], stop.color[1], stop.color[2], 255] };
        triangle(&mut cv, x, base, TRIANGLES - 4, fill, OUTLINE);
    }
    let (w, h) = cv.size();
    (w, h, cv.into_pixels())
}

/// An upward-pointing triangle with its apex at `x, y`, `size` tall.
fn triangle(cv: &mut Canvas, x: i64, y: i64, size: i64, fill: [u8; 4], edge: [u8; 4]) {
    for row in 0..size {
        let half = row * 3 / 4;
        for dx in -half..=half {
            cv.put(x + dx, y + row, fill);
        }
        cv.put(x - half, y + row, edge);
        cv.put(x + half, y + row, edge);
    }
    cv.line(x - (size - 1) * 3 / 4, y + size - 1, x + (size - 1) * 3 / 4, y + size - 1, edge);
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSON: &str = r##"{"ramp": [[0, "#000000ff"], [0.5, "#ff8800ff"], [1, "#ffffffff"]]}"##;
    const CPP: &str =
        r##"Stop ramp[] = { {0.0f, "#000000"}, {0.5f, "#ff8800"}, {1.0f, "#ffffff"} };"##;

    #[test]
    fn find_takes_a_list_of_number_and_colour_pairs() {
        let at = JSON.find("0.5").unwrap();
        let lit = find(JSON, at).expect("found");
        assert_eq!(lit.stops.len(), 3);
        assert_eq!(
            &JSON[lit.open..=lit.close],
            r##"[[0, "#000000ff"], [0.5, "#ff8800ff"], [1, "#ffffffff"]]"##
        );
        assert_eq!(&JSON[lit.stops[1].number.start..lit.stops[1].number.end], "0.5");
        assert_eq!(&JSON[lit.stops[1].color.start..lit.stops[1].color.end], "#ff8800ff");
        assert!(lit.stops[1].color.alpha);
        let g = read(JSON, &lit);
        assert_eq!(g.stops[1], Stop { t: 0.5, color: [255, 136, 0, 255] });
        let lit = find(CPP, CPP.find("0.5f").unwrap()).expect("found");
        assert_eq!(lit.stops.len(), 3);
        assert_eq!(&CPP[lit.stops[0].start..lit.stops[0].end], r##"{0.0f, "#000000"}"##);
        assert_eq!(read(CPP, &lit).stops[2].t, 1.0);
        assert_eq!(find(JSON, 0), None, "outside");
        assert_eq!(
            find("[[0, 0, 1, 0, true], [1, 1, 1, 1, true]]", 3),
            None,
            "a curve is not a gradient"
        );
        assert_eq!(find("[[0, \"#ff0000\", 3]]", 3), None, "three tokens are not a stop");
    }

    #[test]
    fn eval_is_the_stop_the_mix_and_flat_outside() {
        let g = read(JSON, &find(JSON, JSON.find("0.5").unwrap()).unwrap());
        assert_eq!(eval(&g, 0.5), [255, 136, 0, 255]);
        assert_eq!(eval(&g, 0.25), [128, 68, 0, 255]);
        assert_eq!(eval(&g, -1.0), [0, 0, 0, 255]);
        assert_eq!(eval(&g, 2.0), [255, 255, 255, 255]);
        let unsorted = Gradient {
            stops: vec![Stop { t: 1.0, color: [255; 4] }, Stop { t: 0.0, color: [0, 0, 0, 255] }],
        };
        assert_eq!(eval(&unsorted, 0.0), [0, 0, 0, 255], "read sorted whatever the order");
        assert_eq!(sorted_order(&unsorted), [1, 0]);
        assert_eq!(linear().stops.len(), 2);
    }

    #[test]
    fn stop_text_rewrites_the_number_and_the_digits_in_place() {
        let lit = find(CPP, CPP.find("0.5f").unwrap()).unwrap();
        assert_eq!(
            stop_text(CPP, &lit.stops[1], 0.75, [1, 2, 3, 255], 0.05),
            r##"{0.75f, "#010203"}"##
        );
        let lit = find(JSON, JSON.find("0.5").unwrap()).unwrap();
        assert_eq!(
            stop_text(JSON, &lit.stops[0], 0.1, [1, 2, 3, 4], 0.05),
            r##"[0.1, "#01020304"]"##
        );
    }

    #[test]
    fn render_is_the_bar_and_a_triangle_per_stop() {
        let g = read(JSON, &find(JSON, JSON.find("0.5").unwrap()).unwrap());
        let (w, h, px) = render(&g, 1);
        assert_eq!(w as i64, WIDTH + 2 * GAP);
        assert_eq!(h as i64, GAP + BAR + TRIANGLES + GAP);
        let at = |x: i64, y: i64| {
            let i = ((y as u32 * w + x as u32) * 4) as usize;
            [px[i], px[i + 1], px[i + 2]]
        };
        assert_eq!(at(GAP, GAP + BAR / 2), [0, 0, 0]);
        assert_eq!(at(GAP + WIDTH - 1, GAP + BAR / 2), [255, 255, 255]);
        let mid = GAP + (WIDTH - 1) / 2;
        assert_eq!(at(mid, GAP + BAR + 2 + 6), [255, 255, 255], "the selected triangle is white");
        assert_eq!(at(GAP + WIDTH - 1, GAP + BAR + 2 + 6), [255, 255, 255]);
        assert_eq!(at(GAP + 2, GAP + BAR + 2 + 8), [0, 0, 0], "the first stop's triangle is black");
    }
}
