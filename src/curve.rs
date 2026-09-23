//! The curve editor's pure half: a point list in code, found by a bracket
//! scan, read through a layout, rewritten one token at a time, evaluated
//! as a hermite curve and drawn into RGBA. No editor in here. See
//! `docs/specs/curve.md`.

use crate::canvas::{Canvas, text_width};
/// What one number of a point means. `Skip` is the `_` of a layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    X,
    Y,
    Out,
    In,
    Locked,
    Skip,
}

impl Field {
    pub fn name(self) -> &'static str {
        match self {
            Field::X => "x",
            Field::Y => "y",
            Field::Out => "out",
            Field::In => "in",
            Field::Locked => "locked",
            Field::Skip => "_",
        }
    }
}

/// The order a point's numbers come in: `x,y,out,in,locked` by default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout(pub Vec<Field>);

impl Default for Layout {
    fn default() -> Self {
        Layout(vec![Field::X, Field::Y, Field::Out, Field::In, Field::Locked])
    }
}

impl Layout {
    pub fn parse(s: &str) -> Result<Layout, String> {
        let mut fields = Vec::new();
        for part in s.split(',') {
            fields.push(match part.trim() {
                "x" => Field::X,
                "y" => Field::Y,
                "out" => Field::Out,
                "in" => Field::In,
                "locked" => Field::Locked,
                "_" => Field::Skip,
                other => {
                    return Err(format!("not a field: {other} (want x, y, out, in, locked or _)"));
                }
            });
        }
        if !fields.contains(&Field::X) || !fields.contains(&Field::Y) {
            return Err("a layout needs x and y".into());
        }
        Ok(Layout(fields))
    }

    pub fn text(&self) -> String {
        self.0.iter().map(|f| f.name()).collect::<Vec<_>>().join(",")
    }

    /// Which token of a point carries `field`, if the layout names it.
    pub fn index_of(&self, field: Field) -> Option<usize> {
        self.0.iter().position(|&f| f == field)
    }
}

/// One key of the curve. Tangents are slopes, `dy/dx`; `locked` says the
/// two move together.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    pub x: f32,
    pub y: f32,
    pub out: f32,
    pub in_: f32,
    pub locked: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Curve {
    /// In the order the text has them; kept sorted by `x` by whoever edits.
    pub points: Vec<Point>,
}

/// A number or bool in the text, by byte offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub start: usize,
    pub end: usize,
}

/// One point's group in the text — `{…}`, `(…)` or `[…]` — with its
/// number and bool tokens in text order, nested groups flattened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointSpan {
    /// The opening bracket.
    pub start: usize,
    /// One past the closing bracket.
    pub end: usize,
    pub tokens: Vec<Token>,
}

/// The whole list: its brackets and its points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Literal {
    pub open: usize,
    pub close: usize,
    pub points: Vec<PointSpan>,
}

/// A bracket group in the text, with the tokens directly inside it and
/// the groups nested in it.
#[derive(Debug, Default)]
struct Group {
    open: usize,
    close: usize,
    tokens: Vec<Token>,
    children: Vec<Group>,
}

impl Group {
    /// Every token in this group and under it, in text order.
    fn flatten(&self, out: &mut Vec<Token>) {
        let mut children = self.children.iter().peekable();
        for &token in &self.tokens {
            while let Some(child) = children.peek() {
                if child.open < token.start {
                    child.flatten(out);
                    children.next();
                } else {
                    break;
                }
            }
            out.push(token);
        }
        for child in children {
            child.flatten(out);
        }
    }

    fn flat_tokens(&self) -> Vec<Token> {
        let mut out = Vec::new();
        self.flatten(&mut out);
        out
    }

    /// A list of points: only groups directly inside, each with at least
    /// two numbers.
    fn is_list(&self) -> bool {
        !self.children.is_empty()
            && self.tokens.is_empty()
            && self.children.iter().all(|c| c.flat_tokens().len() >= 2)
    }

    fn contains(&self, at: usize) -> bool {
        self.open <= at && at <= self.close
    }
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// The number, if one starts at `at`: an optional sign, digits with an
/// optional fraction and exponent, and a glued suffix of letters and
/// underscores.
pub(crate) fn number_at(bytes: &[u8], at: usize) -> Option<usize> {
    let mut i = at;
    if i < bytes.len() && (bytes[i] == b'-' || bytes[i] == b'+') {
        i += 1;
    }
    let digits = |i: &mut usize| {
        let from = *i;
        while *i < bytes.len() && bytes[*i].is_ascii_digit() {
            *i += 1;
        }
        *i > from
    };
    let whole = digits(&mut i);
    let mut frac = false;
    if i < bytes.len() && bytes[i] == b'.' {
        let mut j = i + 1;
        if digits(&mut j) {
            frac = true;
            i = j;
        } else if whole {
            // `1.` is a number; `1.f` too.
            i = j;
            frac = true;
        }
    }
    if !whole && !frac {
        return None;
    }
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        let mut j = i + 1;
        if j < bytes.len() && (bytes[j] == b'-' || bytes[j] == b'+') {
            j += 1;
        }
        if digits(&mut j) {
            i = j;
        }
    }
    while i < bytes.len() && (bytes[i].is_ascii_alphabetic() || bytes[i] == b'_') {
        i += 1;
    }
    Some(i)
}

/// The group tree over the whole text. Strings and line comments are
/// skipped, so a bracket in them does not count.
fn parse_groups(text: &str) -> Group {
    let bytes = text.as_bytes();
    let mut stack: Vec<Group> = vec![Group { open: 0, close: bytes.len(), ..Default::default() }];
    let mut i = 0;
    let mut prev_ident = false;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'"' | b'\'' => {
                let quote = b;
                i += 1;
                while i < bytes.len() && bytes[i] != quote {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
                prev_ident = false;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                prev_ident = false;
            }
            b'#' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                prev_ident = false;
            }
            b'{' | b'[' | b'(' => {
                stack.push(Group { open: i, close: bytes.len(), ..Default::default() });
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
                prev_ident = false;
            }
            _ if b.is_ascii_digit() || ((b == b'-' || b == b'+' || b == b'.') && !prev_ident) => {
                match number_at(bytes, i) {
                    Some(end) => {
                        stack.last_mut().unwrap().tokens.push(Token { start: i, end });
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
                let start = i;
                while i < bytes.len() && is_ident(bytes[i]) {
                    i += 1;
                }
                let word = &text[start..i];
                if word == "true" || word == "false" {
                    stack.last_mut().unwrap().tokens.push(Token { start, end: i });
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

/// The innermost list of points around byte `cursor`: the innermost
/// group containing it whose direct children are groups each holding at
/// least two numbers.
pub fn find(text: &str, cursor: usize) -> Option<Literal> {
    let root = parse_groups(text);
    let mut best: Option<&Group> = None;
    let mut node = &root;
    loop {
        // The root is never a list: it has no brackets to anchor on.
        if !std::ptr::eq(node, &root) && node.is_list() {
            best = Some(node);
        }
        match node.children.iter().find(|c| c.contains(cursor)) {
            Some(child) => node = child,
            None => break,
        }
    }
    let group = best?;
    Some(Literal {
        open: group.open,
        close: group.close,
        points: group
            .children
            .iter()
            .map(|c| PointSpan {
                start: span_start(text, c.open),
                end: c.close + 1,
                tokens: c.flat_tokens(),
            })
            .collect(),
    })
}

/// An empty list around byte `cursor`: the innermost group containing it
/// that holds nothing at all. `(open, close)` of its brackets.
pub fn find_empty(text: &str, cursor: usize) -> Option<(usize, usize)> {
    let root = parse_groups(text);
    let mut node = &root;
    while let Some(child) = node.children.iter().find(|c| c.contains(cursor)) {
        node = child;
    }
    let empty = !std::ptr::eq(node, &root)
        && node.children.is_empty()
        && node.tokens.is_empty()
        && text[node.open + 1..node.close].trim().is_empty();
    empty.then_some((node.open, node.close))
}

/// The linear preset: `(0, 0)` and `(1, 1)` with the slopes of the line,
/// both locked. What an empty list is seeded with.
pub fn linear() -> Curve {
    Curve {
        points: vec![
            Point { x: 0.0, y: 0.0, out: 1.0, in_: 0.0, locked: true },
            Point { x: 1.0, y: 1.0, out: 1.0, in_: 1.0, locked: true },
        ],
    }
}

/// The text that fills an empty list at `open..=close` with the linear
/// preset, spelled through `layout`: `{0.0, 0.0, 1.0, 0.0, true}` and its
/// partner, one per line when the brackets are on different lines,
/// inline otherwise. Inner brackets are braces inside braces and
/// parentheses inside anything else.
pub fn initial_text(text: &str, open: usize, close: usize, layout: &Layout) -> String {
    let (l, r) = if text.as_bytes()[open] == b'{' { ("{", "}") } else { ("(", ")") };
    let point = |p: &Point| {
        let fields: Vec<String> = layout
            .0
            .iter()
            .map(|f| match f {
                Field::X => format!("{:.1}", p.x),
                Field::Y => format!("{:.1}", p.y),
                Field::Out => format!("{:.1}", p.out),
                Field::In => format!("{:.1}", p.in_),
                Field::Locked => if p.locked { "true" } else { "false" }.to_string(),
                Field::Skip => "0".to_string(),
            })
            .collect();
        format!("{l}{}{r}", fields.join(", "))
    };
    let curve = linear();
    let (a, b) = (point(&curve.points[0]), point(&curve.points[1]));
    if text[open..close].contains('\n') {
        let line_start = text[..open].rfind('\n').map_or(0, |i| i + 1);
        let indent: String =
            text[line_start..open].chars().take_while(|c| c.is_whitespace()).collect();
        format!("\n{indent}    {a},\n{indent}    {b},\n{indent}")
    } else {
        format!("{a}, {b}")
    }
}

/// Where a point's text starts: at its bracket, or at the name glued to
/// the bracket — `Point { … }`, `Vec2(…)` — so a copy of it keeps the name.
pub(crate) fn span_start(text: &str, open: usize) -> usize {
    let bytes = text.as_bytes();
    let mut i = open;
    while i > 0 && (bytes[i - 1] == b' ' || bytes[i - 1] == b'\t') {
        i -= 1;
    }
    let ident_end = i;
    while i > 0 && is_ident(bytes[i - 1]) {
        i -= 1;
    }
    if i == ident_end || bytes[i].is_ascii_digit() { open } else { i }
}

/// The numeric part of a token — what is left once a glued suffix like
/// `f` or `_f32` is taken off.
pub fn numeric_part(token: &str) -> &str {
    let mut end = token.len();
    while end > 0 {
        if token[..end].parse::<f64>().is_ok() {
            return &token[..end];
        }
        end -= 1;
    }
    token
}

fn parse_number(token: &str) -> f32 {
    numeric_part(token).parse::<f32>().unwrap_or(0.0)
}

fn parse_bool(token: &str) -> bool {
    matches!(token, "true") || parse_number(token) != 0.0
}

/// The curve the literal spells, through `layout`. A point shorter than
/// the layout has zeroes where it stops.
pub fn read(text: &str, lit: &Literal, layout: &Layout) -> Curve {
    let points = lit
        .points
        .iter()
        .map(|span| {
            let mut p = Point::default();
            for (i, field) in layout.0.iter().enumerate() {
                let Some(token) = span.tokens.get(i) else { break };
                let tok = &text[token.start..token.end];
                match field {
                    Field::X => p.x = parse_number(tok),
                    Field::Y => p.y = parse_number(tok),
                    Field::Out => p.out = parse_number(tok),
                    Field::In => p.in_ = parse_number(tok),
                    Field::Locked => p.locked = parse_bool(tok),
                    Field::Skip => {}
                }
            }
            p
        })
        .collect();
    Curve { points }
}

// ---- writing back ---------------------------------------------------------

/// How many decimals a step needs: `0.01` two, `0.25` two, `1` none.
pub fn decimals_for(step: f32) -> usize {
    (0..=6usize)
        .find(|&d| {
            let scaled = step as f64 * 10f64.powi(d as i32);
            (scaled - scaled.round()).abs() < 1e-6
        })
        .unwrap_or(6)
}

/// `token` respelled as `value`: its suffix kept, at least as many
/// decimals as it had and never fewer than `step` needs.
pub fn rewrite(token: &str, value: f32, step: f32) -> String {
    let num = numeric_part(token);
    let suffix = &token[num.len()..];
    let own = num
        .split_once('.')
        .map(|(_, frac)| frac.chars().take_while(char::is_ascii_digit).count())
        .unwrap_or(0);
    let decimals = own.max(decimals_for(step));
    let mut text = format!("{:.*}", decimals, value as f64);
    if decimals > own {
        let mut keep = decimals;
        while keep > own && text.ends_with('0') {
            text.pop();
            keep -= 1;
        }
        if text.ends_with('.') {
            text.pop();
        }
    }
    if text == "-0" || text.starts_with("-0.") && text[1..].bytes().all(|b| b == b'0' || b == b'.')
    {
        text.remove(0);
    }
    text.push_str(suffix);
    text
}

/// A bool token respelled, `true`/`false` or `1`/`0` as it was.
pub fn rewrite_bool(token: &str, value: bool) -> String {
    match token {
        "true" | "false" => if value { "true" } else { "false" }.into(),
        _ => if value { "1" } else { "0" }.into(),
    }
}

/// The text of a new point shaped like `like`: its brackets, separators
/// and names copied, the layout's fields set from `p`.
pub fn point_text(text: &str, like: &PointSpan, layout: &Layout, p: Point, step: f32) -> String {
    let mut out = text[like.start..like.end].to_string();
    let base = like.start;
    for (i, field) in layout.0.iter().enumerate().rev() {
        let Some(token) = like.tokens.get(i) else { continue };
        let old = &text[token.start..token.end];
        let new = match field {
            Field::X => rewrite(old, p.x, step),
            Field::Y => rewrite(old, p.y, step),
            Field::Out => rewrite(old, p.out, step),
            Field::In => rewrite(old, p.in_, step),
            Field::Locked => rewrite_bool(old, p.locked),
            Field::Skip => continue,
        };
        out.replace_range(token.start - base..token.end - base, &new);
    }
    out
}

// ---- evaluation -----------------------------------------------------------

/// The segment `x` falls in: the last point at or before it and the first
/// strictly after, skipping zero-length segments.
fn segment(curve: &Curve, x: f32) -> Option<(Point, Point)> {
    let pts = &curve.points;
    for i in 0..pts.len().saturating_sub(1) {
        let (p, q) = (pts[i], pts[i + 1]);
        if q.x > p.x && p.x <= x && x < q.x {
            return Some((p, q));
        }
    }
    None
}

/// The curve's value at `x`: cubic hermite between neighbours, Unity's
/// way — tangents are slopes scaled by the segment length — and flat
/// past either end.
pub fn eval(curve: &Curve, x: f32) -> f32 {
    let pts = &curve.points;
    let Some(first) = pts.first() else { return 0.0 };
    let last = pts[pts.len() - 1];
    if x <= first.x {
        return first.y;
    }
    if x >= last.x {
        return last.y;
    }
    let Some((p, q)) = segment(curve, x) else { return last.y };
    let d = q.x - p.x;
    let t = (x - p.x) / d;
    let (t2, t3) = (t * t, t * t * t);
    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
    let h10 = t3 - 2.0 * t2 + t;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;
    h00 * p.y + h10 * d * p.out + h01 * q.y + h11 * d * q.in_
}

/// `dy/dx` at `x`; zero outside the range, where the curve is flat.
pub fn slope(curve: &Curve, x: f32) -> f32 {
    let Some((p, q)) = segment(curve, x) else { return 0.0 };
    let d = q.x - p.x;
    let t = (x - p.x) / d;
    let t2 = t * t;
    let h00 = 6.0 * t2 - 6.0 * t;
    let h10 = 3.0 * t2 - 4.0 * t + 1.0;
    let h01 = -6.0 * t2 + 6.0 * t;
    let h11 = 3.0 * t2 - 2.0 * t;
    (h00 * p.y + h10 * d * p.out + h01 * q.y + h11 * d * q.in_) / d
}

/// The plot's range: the unit square, stretched to take in any point
/// outside it.
pub fn plot_range(curve: &Curve) -> ((f32, f32), (f32, f32)) {
    let mut r = ((0.0f32, 1.0f32), (0.0f32, 1.0f32));
    for p in &curve.points {
        if p.x.is_finite() {
            r.0 = (r.0.0.min(p.x), r.0.1.max(p.x));
        }
        if p.y.is_finite() {
            r.1 = (r.1.0.min(p.y), r.1.1.max(p.y));
        }
    }
    r
}

// ---- the picture ----------------------------------------------------------

const GRID: [u8; 4] = [44, 44, 52, 255];
const AXIS: [u8; 4] = [96, 96, 108, 255];
const LABEL: [u8; 4] = [150, 150, 160, 255];
const LINE: [u8; 4] = [110, 190, 255, 255];
const DOT: [u8; 4] = [235, 235, 240, 255];
const PICK: [u8; 4] = [255, 180, 60, 255];
const HANDLE_COLOR: [u8; 4] = [255, 210, 130, 255];

/// One grid line's drawing: the canvas, the value, its colour, whether
/// it gets a label.
type Tick<'a> = Box<dyn FnMut(&mut Canvas, f32, [u8; 4], bool) + 'a>;

/// Pixels per unit: the `0,0` to `1,1` square is always this size.
pub const UNIT: f32 = 512.0;
/// The picture never grows past this on a side, however far a point runs.
const MAX_SIDE: f32 = 4096.0;
const MARGIN_LEFT: i64 = 30;
const MARGIN_RIGHT: i64 = 10;
const MARGIN_TOP: i64 = 10;
const MARGIN_BOTTOM: i64 = 16;
/// A tangent handle's length, in units — the engine's own.
const HANDLE: f32 = 0.12;

/// The plot: the unit square at `UNIT` pixels a side, stretched to take in
/// any point outside it; a grid every tenth, labels every fifth, the
/// curve, every point, the selected one larger with its tangents as the
/// engine draws them. Returns `(width, height, rgba)`.
pub fn render(curve: &Curve, selected: usize) -> (u32, u32, Vec<u8>) {
    let ((x0, x1), (y0, y1)) = plot_range(curve);
    let scale_x = (UNIT).min(MAX_SIDE / (x1 - x0).max(1e-6));
    let scale_y = (UNIT).min(MAX_SIDE / (y1 - y0).max(1e-6));
    let iw = ((x1 - x0) * scale_x).round().max(1.0) as i64;
    let ih = ((y1 - y0) * scale_y).round().max(1.0) as i64;
    let (w, h) = (iw + MARGIN_LEFT + MARGIN_RIGHT, ih + MARGIN_TOP + MARGIN_BOTTOM);
    let mut cv = Canvas::new(w as u32, h as u32);
    let (inner_l, inner_r) = (MARGIN_LEFT, MARGIN_LEFT + iw);
    let (inner_t, inner_b) = (MARGIN_TOP, MARGIN_TOP + ih);
    let sx = |x: f32| inner_l as f32 + (x - x0) * scale_x;
    let sy = |y: f32| inner_b as f32 - (y - y0) * scale_y;

    // The grid: a line every tenth, the whole numbers and the square's
    // edges brighter, a label every fifth.
    let mut tick = |lo: f32, hi: f32, mut draw: Tick| {
        let mut n = (lo * 10.0).floor() as i64;
        while (n as f32) / 10.0 <= hi + 1e-4 {
            let t = n as f32 / 10.0;
            if t >= lo - 1e-4 {
                let bright = n % 10 == 0;
                draw(&mut cv, t, if bright { AXIS } else { GRID }, n % 2 == 0);
            }
            n += 1;
        }
    };
    tick(
        y0,
        y1,
        Box::new(move |cv, t, c, label| {
            let row = sy(t).round() as i64;
            cv.line(inner_l, row, inner_r, row, c);
            if label {
                let text = format!("{t:.1}");
                let lw = text_width(&text);
                cv.text(inner_l - 4 - lw, row - 2, &text, LABEL);
            }
        }),
    );
    tick(
        x0,
        x1,
        Box::new(move |cv, t, c, label| {
            let col = sx(t).round() as i64;
            cv.line(col, inner_t, col, inner_b, c);
            if label {
                let text = format!("{t:.1}");
                let lw = text_width(&text);
                cv.text(col - lw / 2, inner_b + 4, &text, LABEL);
            }
        }),
    );

    // The curve, one sample per column.
    if !curve.points.is_empty() {
        let mut prev: Option<(i64, i64)> = None;
        for col in inner_l..=inner_r {
            let x = x0 + (col - inner_l) as f32 / scale_x;
            let row = sy(eval(curve, x)).round() as i64;
            if let Some((pc, pr)) = prev {
                cv.line(pc, pr, col, row, LINE);
                cv.line(pc, pr + 1, col, row + 1, LINE);
            }
            prev = Some((col, row));
        }
    }
    // Points, the selected one last so it sits on top.
    for (i, p) in curve.points.iter().enumerate() {
        if i != selected {
            cv.disc(sx(p.x).round() as i64, sy(p.y).round() as i64, 3, DOT);
        }
    }
    if let Some(p) = curve.points.get(selected) {
        let (cx, cy) = (sx(p.x).round() as i64, sy(p.y).round() as i64);
        // The engine's handles: `normalize(1, tangent) * 0.12` from the
        // anchor, out to the right and in to the left.
        for (m, dir) in [(p.out, 1.0f32), (p.in_, -1.0f32)] {
            let len = (1.0 + m * m).sqrt();
            let (hx, hy) = (p.x + dir * HANDLE / len, p.y + dir * m * HANDLE / len);
            let (ex, ey) = (sx(hx).round() as i64, sy(hy).round() as i64);
            cv.line(cx, cy, ex, ey, HANDLE_COLOR);
            cv.disc(ex, ey, 2, HANDLE_COLOR);
        }
        cv.disc(cx, cy, 5, PICK);
    }
    (w as u32, h as u32, cv.into_pixels())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CPP: &str = "std::vector<Point> damage = {\n    {0.0f, 0.0f, 1.0f, 1.0f, false},\n    {0.5f, 0.8f, 0.0f, 0.0f, true},\n    {1.0f, 1.0f, 1.0f, 1.0f, false},\n};\n";
    const RUST: &str = "let c = vec![Point { x: 0.0, y: 0.0, out: 1.0, in_: 1.0, locked: false }, Point { x: 0.5, y: 0.8, out: 0.0, in_: 0.0, locked: true }, Point { x: 1.0, y: 1.0, out: 1.0, in_: 1.0, locked: false }];";
    const PY: &str = "curve = [(0.0, 0.0), (0.5, 0.8), (1.0, 1.0)]\n";

    #[test]
    fn find_takes_the_list_around_the_cursor() {
        let at = CPP.find("0.8f").unwrap();
        let lit = find(CPP, at).expect("a curve");
        assert_eq!(lit.points.len(), 3);
        assert_eq!(&CPP[lit.open..=lit.open], "{");
        assert_eq!(&CPP[lit.close..=lit.close], "}");
        assert_eq!(lit.points[1].tokens.len(), 5);
        let t = &lit.points[1].tokens[1];
        assert_eq!(&CPP[t.start..t.end], "0.8f");
        assert_eq!(&CPP[lit.points[1].start..lit.points[1].end], "{0.5f, 0.8f, 0.0f, 0.0f, true}");
    }

    #[test]
    fn find_from_inside_a_point_takes_the_outer_list() {
        let at = CPP.find("0.8f").unwrap() + 1;
        assert_eq!(find(CPP, at).unwrap().points.len(), 3);
    }

    #[test]
    fn find_from_the_list_bracket_itself_works() {
        let at = CPP.find("= {").unwrap() + 2;
        assert_eq!(find(CPP, at).unwrap().points.len(), 3);
    }

    #[test]
    fn find_outside_any_list_is_none() {
        assert!(find(CPP, 3).is_none());
        assert!(find("int x = 1;", 4).is_none());
        assert!(find("f(1, 2, 3)", 3).is_none(), "numbers, but no groups of them");
    }

    #[test]
    fn find_reads_rust_struct_literals_and_python_tuples() {
        let lit = find(RUST, RUST.find("0.8").unwrap()).unwrap();
        assert_eq!(lit.points.len(), 3);
        assert_eq!(lit.points[0].tokens.len(), 5, "identifiers and colons are ignored");
        let lit = find(PY, PY.find("0.8").unwrap()).unwrap();
        assert_eq!(lit.points.len(), 3);
        assert_eq!(lit.points[0].tokens.len(), 2);
    }

    #[test]
    fn find_skips_strings_and_comments() {
        let text = "// {1, 2}\nlet s = \"(3, 4)\";\nlet c = [(0.0, 0.0), (1.0, 1.0)];";
        let lit = find(text, text.find("1.0").unwrap()).unwrap();
        assert_eq!(lit.points.len(), 2);
    }

    #[test]
    fn read_maps_the_default_layout() {
        let lit = find(CPP, CPP.find("0.8f").unwrap()).unwrap();
        let c = read(CPP, &lit, &Layout::default());
        assert_eq!(c.points[1], Point { x: 0.5, y: 0.8, out: 0.0, in_: 0.0, locked: true });
        assert!(!c.points[0].locked);
        assert_eq!(c.points[0].out, 1.0);
    }

    #[test]
    fn read_with_a_short_layout_and_a_skip() {
        let lit = find(PY, PY.find("0.8").unwrap()).unwrap();
        let c = read(PY, &lit, &Layout::parse("x,y").unwrap());
        assert_eq!(c.points[1], Point { x: 0.5, y: 0.8, ..Point::default() });
        let text = "[(9, 0.0, 0.0), (9, 0.5, 0.8), (9, 1.0, 1.0)]";
        let lit = find(text, 10).unwrap();
        let c = read(text, &lit, &Layout::parse("_,x,y").unwrap());
        assert_eq!(c.points[1].x, 0.5);
    }

    #[test]
    fn read_a_short_point_has_zero_tangents_and_one_reads_as_locked() {
        let text = "{ {0.0, 0.0}, {1.0, 1.0, 2.0, 2.0, 1} }";
        let lit = find(text, 4).unwrap();
        let c = read(text, &lit, &Layout::default());
        assert_eq!(c.points[0], Point { x: 0.0, y: 0.0, ..Point::default() });
        assert!(c.points[1].locked);
    }

    #[test]
    fn negative_numbers_and_exponents_read() {
        let text = "[(-1.0, 1e-3), (2, -2.5e2)]";
        let lit = find(text, 3).unwrap();
        let c = read(text, &lit, &Layout::parse("x,y").unwrap());
        assert_eq!(c.points[0], Point { x: -1.0, y: 0.001, ..Point::default() });
        assert_eq!(c.points[1], Point { x: 2.0, y: -250.0, ..Point::default() });
    }

    #[test]
    fn layout_parses_and_refuses() {
        assert_eq!(Layout::default().text(), "x,y,out,in,locked");
        assert_eq!(Layout::parse("y, x").unwrap().text(), "y,x");
        assert_eq!(
            Layout::parse("x,z").unwrap_err(),
            "not a field: z (want x, y, out, in, locked or _)"
        );
        assert_eq!(Layout::parse("out,in").unwrap_err(), "a layout needs x and y");
    }
    #[test]
    fn rewrite_keeps_suffix_sign_and_decimals() {
        assert_eq!(rewrite("0.5f", 0.51, 0.01), "0.51f");
        assert_eq!(rewrite("0.50f", 0.51, 0.01), "0.51f");
        assert_eq!(rewrite("0.500", 0.51, 0.01), "0.510", "keeps its three");
        assert_eq!(rewrite("1", 1.25, 0.25), "1.25");
        assert_eq!(rewrite("1", 2.0, 1.0), "2");
        assert_eq!(rewrite("0.5f", 0.6, 0.01), "0.6f", "no zeros past its own");
        assert_eq!(rewrite("-2.0_f32", -1.9, 0.1), "-1.9_f32");
        assert_eq!(rewrite("1e-3", 0.002, 0.001), "0.002");
        assert_eq!(rewrite("0.01", 0.0, 0.01), "0.00", "never minus zero");
        assert_eq!(rewrite_bool("true", false), "false");
        assert_eq!(rewrite_bool("1", false), "0");
        assert_eq!(decimals_for(0.01), 2);
        assert_eq!(decimals_for(0.25), 2);
        assert_eq!(decimals_for(1.0), 0);
    }

    #[test]
    fn a_new_point_copies_its_neighbours_shape() {
        let lit = find(CPP, CPP.find("0.8f").unwrap()).unwrap();
        let p = Point { x: 0.25, y: 0.4, out: 1.5, in_: 1.5, locked: true };
        let text = point_text(CPP, &lit.points[1], &Layout::default(), p, 0.01);
        assert_eq!(text, "{0.25f, 0.4f, 1.5f, 1.5f, true}");
        let lit = find(RUST, RUST.find("0.8").unwrap()).unwrap();
        let text = point_text(RUST, &lit.points[1], &Layout::default(), p, 0.01);
        assert_eq!(text, "Point { x: 0.25, y: 0.4, out: 1.5, in_: 1.5, locked: true }");
    }

    fn c(points: &[(f32, f32, f32, f32)]) -> Curve {
        Curve {
            points: points
                .iter()
                .map(|&(x, y, out, in_)| Point { x, y, out, in_, locked: false })
                .collect(),
        }
    }

    #[test]
    fn eval_hits_points_and_is_flat_outside() {
        let curve = c(&[(0.0, 0.0, 0.0, 0.0), (1.0, 1.0, 0.0, 0.0)]);
        assert_eq!(eval(&curve, 0.0), 0.0);
        assert_eq!(eval(&curve, 1.0), 1.0);
        assert!((eval(&curve, 0.5) - 0.5).abs() < 1e-6, "smooth step's middle");
        assert!((eval(&curve, 0.25) - 0.15625).abs() < 1e-5, "3t²-2t³ at 0.25");
        assert_eq!(eval(&curve, -1.0), 0.0);
        assert_eq!(eval(&curve, 2.0), 1.0);
        assert_eq!(eval(&Curve::default(), 0.5), 0.0);
    }

    #[test]
    fn eval_with_matching_slopes_is_the_line() {
        let curve = c(&[(0.0, 0.0, 1.0, 1.0), (2.0, 2.0, 1.0, 1.0)]);
        for i in 0..=8 {
            let x = i as f32 * 0.25;
            assert!((eval(&curve, x) - x).abs() < 1e-5, "at {x}");
        }
        assert!((slope(&curve, 1.0) - 1.0).abs() < 1e-4);
        assert_eq!(slope(&curve, 5.0), 0.0);
    }

    #[test]
    fn eval_skips_a_zero_length_segment() {
        let curve = c(&[
            (0.0, 0.0, 0.0, 0.0),
            (0.5, 1.0, 0.0, 0.0),
            (0.5, 2.0, 0.0, 0.0),
            (1.0, 3.0, 0.0, 0.0),
        ]);
        assert!(eval(&curve, 0.5).is_finite());
        assert!(eval(&curve, 0.75).is_finite());
        assert!(eval(&curve, 0.25).is_finite());
    }

    #[test]
    fn plot_range_is_the_unit_square_stretched_to_the_points() {
        let curve = c(&[(0.0, 0.5, 0.0, 0.0), (1.0, 0.5, 0.0, 0.0)]);
        assert_eq!(plot_range(&curve), ((0.0, 1.0), (0.0, 1.0)));
        let wide = c(&[(-0.5, 0.0, 0.0, 0.0), (1.0, 1.5, 0.0, 0.0)]);
        assert_eq!(plot_range(&wide), ((-0.5, 1.0), (0.0, 1.5)));
        assert_eq!(plot_range(&Curve::default()), ((0.0, 1.0), (0.0, 1.0)));
    }

    #[test]
    fn render_is_the_unit_square_at_a_fixed_size_and_marks_the_selected_point() {
        let curve = c(&[(0.0, 0.0, 0.0, 0.0), (1.0, 1.0, 0.0, 0.0)]);
        let (w, h, px) = render(&curve, 1);
        assert_eq!((w, h), (512 + 40, 512 + 26));
        assert_eq!(px.len(), (w * h * 4) as usize);
        let (_, _, other) = render(&curve, 0);
        assert_ne!(px, other, "the selection shows");
        let tall = c(&[(0.0, 0.0, 0.0, 0.0), (1.0, 2.0, 0.0, 0.0)]);
        let (w2, h2, _) = render(&tall, 0);
        assert_eq!((w2, h2), (w, 1024 + 26), "the square keeps its scale; the picture grows");
        let (w3, h3, px3) = render(&Curve::default(), 0);
        assert_eq!(px3.len(), (w3 * h3 * 4) as usize);
        let far = c(&[(0.0, 0.0, 0.0, 0.0), (1.0, 1e6, 0.0, 0.0)]);
        let (_, h4, _) = render(&far, 0);
        assert!(h4 <= 4096 + 26, "capped: {h4}");
    }

    #[test]
    fn an_empty_list_is_found_and_seeded_with_the_linear_preset() {
        let text = "std::vector<Point> pts = {};\n";
        let at = text.find("{}").unwrap() + 1;
        assert_eq!(find_empty(text, at), Some((at - 1, at)));
        assert_eq!(find_empty(text, 3), None);
        assert_eq!(find_empty("a = { {1, 2} }", 3), None, "not empty");
        assert_eq!(
            initial_text(text, at - 1, at, &Layout::default()),
            "{0.0, 0.0, 1.0, 0.0, true}, {1.0, 1.0, 1.0, 1.0, true}"
        );
        let multi = "    std::vector<Point> pts = {\n    };\n";
        let open = multi.find('{').unwrap();
        let close = multi.find('}').unwrap();
        assert_eq!(find_empty(multi, open + 1), Some((open, close)));
        assert_eq!(
            initial_text(multi, open, close, &Layout::default()),
            "\n        {0.0, 0.0, 1.0, 0.0, true},\n        {1.0, 1.0, 1.0, 1.0, true},\n    "
        );
        let py = "pts = []";
        assert_eq!(
            initial_text(py, 6, 7, &Layout::parse("x,y").unwrap()),
            "(0.0, 0.0), (1.0, 1.0)"
        );
        let seeded = format!(
            "std::vector<Point> pts = {{{}}};",
            initial_text(text, at - 1, at, &Layout::default())
        );
        let lit = find(&seeded, 30).unwrap();
        assert_eq!(read(&seeded, &lit, &Layout::default()), linear());
    }
}
