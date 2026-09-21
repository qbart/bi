//! The curve editor's pure half: a point list in code, found by a bracket
//! scan, read through a layout, rewritten one token at a time, evaluated
//! as a hermite curve and drawn into RGBA. No editor in here. See
//! `docs/specs/curve.md`.

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
    fn name(self) -> &'static str {
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
fn number_at(bytes: &[u8], at: usize) -> Option<usize> {
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
            _ if b.is_ascii_digit()
                || ((b == b'-' || b == b'+' || b == b'.') && !prev_ident) =>
            {
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
            .map(|c| PointSpan { start: c.open, end: c.close + 1, tokens: c.flat_tokens() })
            .collect(),
    })
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
}
