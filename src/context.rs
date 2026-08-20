//! Which block the cursor is in, and the line that opened it.
//!
//! A closing brace says nothing. Twenty lines into a function, inside a loop,
//! inside a match arm, the `}` under the cursor is one of four that look the
//! same. The parse tree already knows which; this turns that into a line of
//! text to hang off the closing row.
//!
//! Producing the decoration is [`crate::editor::Editor::decorations`]; this is
//! the walk it does first. See `docs/specs/tree-sitter-context.md`.

use ropey::Rope;

use crate::syntax::Syntax;

/// One block around the cursor: where it closes, and the line that opened it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    /// The row the block ends on. In C that is the `}`; in Python, which has
    /// no closing line, it is the block's last statement — which is where the
    /// block ends, and inventing a row to hang it on would be worse.
    pub row: usize,
    /// The opening row's text, trimmed at both ends.
    pub opener: String,
}

/// The blocks containing `byte`, innermost first, at most `depth` of them.
///
/// **The line, not the node.** A node's own text starts where the node starts,
/// which for a C `compound_statement` is the `{` — annotating a block with `{`
/// would be a joke. Taking the whole opening row instead means the condition,
/// the function signature and the `match` scrutinee all arrive without one arm
/// per grammar.
///
/// **Nothing while the cursor is on the opening line**: the line being
/// repeated is then already under the cursor, and repeating it three rows down
/// is noise about something in plain view. Sitting on the closing brace still
/// counts, which is the case this exists for.
///
/// **A block's opening row is less indented than what is inside it**, and that
/// is the test for whether a node's first row opened anything. Python's
/// `block` starts at the *first statement* of the `if`, not at the `if`, and
/// the two are indistinguishable to anything that does not name node kinds
/// per grammar — which is thirty query files. Indentation is the same
/// assumption the indent guides already make about the file, and it costs one
/// comparison.
pub fn contexts(
    syntax: &Syntax,
    rope: &Rope,
    byte: usize,
    depth: usize,
    min_lines: usize,
) -> Vec<Context> {
    if depth == 0 {
        return Vec::new();
    }
    let byte = byte.min(rope.len_bytes());
    let cursor_row = rope.byte_to_line(byte);
    // A block must open above the cursor and span at least this many rows.
    // The floor of 1 is not a taste: a block that opens and closes on one row
    // has nothing to say that the row does not already say.
    let span = min_lines.max(1);

    let mut out: Vec<Context> = Vec::new();
    let mut seen: Vec<(usize, usize)> = Vec::new();
    for range in syntax.scopes_at(byte) {
        let start = rope.byte_to_line(range.start);
        // The row of the last byte *in* the node, not of the exclusive end: a
        // node finishing at a line break ends on the row before the one that
        // break starts.
        let end = rope.byte_to_line(range.end.saturating_sub(1).min(rope.len_bytes()));

        if start >= cursor_row || end.saturating_sub(start) < span {
            continue;
        }
        // Row pairs rather than byte ranges. In C, `if (v) { … }` is an
        // `if_statement` whose `compound_statement` child opens and closes on
        // the same two rows: one block wearing two hats, and annotating it
        // twice would print the same line twice on the same brace.
        if seen.contains(&(start, end)) {
            continue;
        }
        seen.push((start, end));

        // Did this row open anything? Python's `block` node begins at the
        // first statement of the `if`, which is a row *inside* the construct
        // and reads as `# print(name)` — the row above is the one that opened
        // it, and it is the less indented of the two.
        let Some(inside) = content_row_after(rope, start, end) else { continue };
        if indent(rope, start) >= indent(rope, inside) {
            continue;
        }

        // The row that opened it, walking up past a row of pure punctuation:
        // in `int main(void)\n{` the block starts on the brace, `} // {` is a
        // joke, and the signature above it is what a reader would have said.
        let Some(named) = named_row_at_or_above(rope, start) else { continue };
        out.push(Context { row: end, opener: line(rope, named) });
        if out.len() == depth {
            break;
        }
    }
    out
}

/// `row`'s text, trimmed at both ends.
fn line(rope: &Rope, row: usize) -> String {
    rope.line(row).to_string().trim().to_string()
}

/// How far `row` is indented, in leading whitespace characters.
///
/// Characters rather than display columns, and a tab counts as one. The only
/// question asked of this is which of two rows in the same block is further
/// in, and a file that indents one of them with tabs and the other with spaces
/// has a bigger problem than this annotation.
fn indent(rope: &Rope, row: usize) -> usize {
    rope.line(row).chars().take_while(|c| *c == ' ' || *c == '\t').count()
}

/// The first row after `start`, up to and including `end`, with something on
/// it. `None` when the block is nothing but blank rows.
fn content_row_after(rope: &Rope, start: usize, end: usize) -> Option<usize> {
    (start + 1..=end).find(|&row| !line(rope, row).is_empty())
}

/// `row`, or the nearest row above it that names something rather than being
/// bare punctuation. `None` when there is no such row.
fn named_row_at_or_above(rope: &Rope, row: usize) -> Option<usize> {
    (0..=row).rev().find(|&row| line(rope, row).chars().any(char::is_alphanumeric))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses `text` as `file` and asks what surrounds the cursor, which is
    /// marked in the text with a `|` that is removed before parsing.
    fn at(file: &str, text: &str, depth: usize, min_lines: usize) -> Vec<Context> {
        let byte = text.find('|').expect("the test marks a cursor");
        let text = text.replace('|', "");
        let rope = Rope::from_str(&text);
        let syntax = Syntax::new(file, &rope).expect("a grammar for the test's file");
        contexts(&syntax, &rope, byte, depth, min_lines)
    }

    fn openers(found: &[Context]) -> Vec<&str> {
        found.iter().map(|c| c.opener.as_str()).collect()
    }

    const C: &str = "\
int main(void) {
    if (value == 0) {
        do_something();
|    }
    return 0;
}
";

    /// The line that opened it, trimmed — not the node's own text, which for
    /// the `compound_statement` here starts at the `{`.
    #[test]
    fn the_innermost_block_gives_the_line_that_opened_it() {
        let found = at("a.c", C, 1, 1);
        assert_eq!(openers(&found), ["if (value == 0) {"]);
        assert_eq!(found[0].row, 3, "hung off the row that closes it");
    }

    /// `if_statement` and `compound_statement` are two nodes over the same
    /// rows. One block, so one annotation.
    #[test]
    fn a_block_wearing_two_nodes_is_annotated_once() {
        let found = at("a.c", C, 4, 1);
        assert_eq!(openers(&found), ["if (value == 0) {", "int main(void) {"]);
    }

    #[test]
    fn depth_counts_outwards_from_the_innermost() {
        assert_eq!(openers(&at("a.c", C, 1, 1)).len(), 1);
        assert_eq!(openers(&at("a.c", C, 2, 1)), ["if (value == 0) {", "int main(void) {"]);
    }

    /// The honest spelling of off.
    #[test]
    fn a_depth_of_zero_is_off() {
        assert!(at("a.c", C, 0, 1).is_empty());
    }

    /// The three-row `if` goes, the six-row function stays.
    #[test]
    fn min_lines_drops_the_short_blocks_and_keeps_the_long_ones() {
        assert_eq!(openers(&at("a.c", C, 4, 4)), ["int main(void) {"]);
    }

    /// The line would be repeated three rows under itself, which is noise
    /// about something in plain view.
    #[test]
    fn the_cursor_on_the_opening_line_says_nothing() {
        let text = "\
int main(void) {
    if (value| == 0) {
        do_something();
    }
}
";
        assert_eq!(openers(&at("a.c", text, 1, 1)), ["int main(void) {"]);
    }

    /// And on the closing row it still speaks, which is the case the whole
    /// feature is for.
    #[test]
    fn the_cursor_on_the_closing_row_still_speaks() {
        let text = "\
int main(void) {
    if (value == 0) {
        do_something();
    |}
}
";
        assert_eq!(openers(&at("a.c", text, 1, 1)), ["if (value == 0) {"]);
    }

    /// Python has no closing line, so the block ends on its last statement.
    #[test]
    fn python_lands_on_the_last_statement_of_the_block() {
        let text = "\
def greet(name):
    if name:
        print(name)
        print|(name)
";
        let found = at("a.py", text, 1, 1);
        assert_eq!(openers(&found), ["if name:"]);
        assert_eq!(found[0].row, 3, "the last row of the block");
    }

    /// Rust nests three ways at once, and a match arm's brace is a block whose
    /// opening row is the arm rather than the `match`.
    #[test]
    fn rust_names_the_arm_then_the_match_then_the_function() {
        let text = "\
fn run(x: u8) {
    match x {
        0 => {
            done|();
        }
        _ => {}
    }
}
";
        assert_eq!(openers(&at("a.rs", text, 4, 1)), ["0 => {", "match x {", "fn run(x: u8) {"],);
    }

    #[test]
    fn lua_closes_on_its_end() {
        let text = "\
function greet(name)
    print|(name)
end
";
        let found = at("a.lua", text, 1, 1);
        assert_eq!(openers(&found), ["function greet(name)"]);
        assert_eq!(found[0].row, 2);
    }

    /// No grammar, no context — the answer `S` gives, for the same reason:
    /// guessing at braces is how an editor tells you confidently about a block
    /// that is inside a string.
    #[test]
    fn a_file_with_no_grammar_has_nothing_to_say() {
        let rope = Rope::from_str("if (x) {\n    y\n}\n");
        assert!(Syntax::new("a.unknownext", &rope).is_none());
    }

    /// A brace on a line of its own opened the block, and `} // {` is a joke.
    /// The walk carries on outwards to the row that names something.
    #[test]
    fn a_row_of_nothing_but_punctuation_is_skipped() {
        let text = "\
int main(void)
{
    do_something|();
}
";
        assert_eq!(openers(&at("a.c", text, 1, 1)), ["int main(void)"]);
    }
}
