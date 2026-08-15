//! Incremental parsing and syntax highlighting.
//!
//! Highlights come out as **capture names** — `keyword`, `string`, `comment` —
//! never as terminal styles. `ui.rs` maps names to colours. That boundary is
//! what keeps the core usable from a non-terminal frontend, and it is the same
//! indirection a theme file will want.
//!
//! See `docs/specs/tree-sitter.md`.

use std::ops::Range;

use ropey::Rope;
use streaming_iterator::StreamingIterator;
use tree_sitter::{
    InputEdit, Language, Node, Parser, Point as TsPoint, Query, QueryCursor, TextProvider, Tree,
};

use crate::buffer::Edit;

/// A highlighted byte range. `capture` indexes into [`Syntax::capture_name`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start_byte: usize,
    pub end_byte: usize,
    pub capture: u32,
}

/// Grammar for a file extension, or `None` for plain text.
///
/// One arm per language — adding one is a line. Rust only for now: the editor
/// is written in Rust so it is what gets dogfooded, and every grammar is a C
/// library that costs build time and binary size.
fn language_for(extension: &str) -> Option<(Language, &'static str)> {
    match extension {
        "rs" => Some((tree_sitter_rust::LANGUAGE.into(), tree_sitter_rust::HIGHLIGHTS_QUERY)),
        _ => None,
    }
}

/// Lets tree-sitter read predicate text straight out of the rope instead of
/// materialising the buffer as a `String`.
struct RopeProvider<'a>(&'a Rope);

impl<'a> TextProvider<&'a [u8]> for RopeProvider<'a> {
    type I = std::iter::Map<ropey::iter::Chunks<'a>, fn(&str) -> &[u8]>;

    fn text(&mut self, node: Node) -> Self::I {
        let range = node.byte_range();
        let start = range.start.min(self.0.len_bytes());
        let end = range.end.min(self.0.len_bytes());
        self.0.byte_slice(start..end).chunks().map(str::as_bytes)
    }
}

pub struct Syntax {
    parser: Parser,
    tree: Tree,
    query: Query,
}

impl Syntax {
    /// Parses `rope` for the grammar matching `extension`. `None` when no
    /// grammar is known — an unrecognised file is plain text, never an error.
    pub fn new(extension: &str, rope: &Rope) -> Option<Self> {
        let (language, highlights) = language_for(extension)?;
        let mut parser = Parser::new();
        parser.set_language(&language).ok()?;
        let query = Query::new(&language, highlights).ok()?;
        let tree = parse(&mut parser, rope, None)?;
        Some(Self { parser, tree, query })
    }

    pub fn capture_name(&self, capture: u32) -> &str {
        self.query.capture_names()[capture as usize]
    }

    /// Feeds `edits` to the old tree and reparses.
    ///
    /// Every edit has to reach `Tree::edit` in order before the reparse —
    /// batching them and parsing once is the intended usage, not a shortcut.
    pub fn update(&mut self, rope: &Rope, edits: &[Edit]) {
        for edit in edits {
            self.tree.edit(&input_edit(edit));
        }
        if let Some(tree) = parse(&mut self.parser, rope, Some(&self.tree)) {
            self.tree = tree;
        }
    }

    /// Non-overlapping highlight spans covering `range`, in order.
    ///
    /// Only the visible byte range is queried, so frame cost stays bounded by
    /// terminal height rather than file size.
    pub fn highlights(&self, rope: &Rope, range: Range<usize>) -> Vec<Span> {
        if range.is_empty() {
            return Vec::new();
        }

        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(range.clone());

        let mut raw: Vec<(usize, usize, u32)> = Vec::new();
        let mut matches = cursor.matches(&self.query, self.tree.root_node(), RopeProvider(rope));
        while let Some(m) = matches.next() {
            for capture in m.captures {
                let node = capture.node.byte_range();
                raw.push((node.start, node.end, capture.index));
            }
        }

        // Widest first, so a nested capture overwrites the one containing it —
        // the innermost match is the specific one.
        raw.sort_by_key(|(start, end, _)| std::cmp::Reverse(end - start));

        let mut cells: Vec<Option<u32>> = vec![None; range.len()];
        for (start, end, capture) in raw {
            let start = start.clamp(range.start, range.end) - range.start;
            let end = end.clamp(range.start, range.end) - range.start;
            for cell in &mut cells[start..end] {
                *cell = Some(capture);
            }
        }

        // Run-length encode back into spans.
        let mut spans: Vec<Span> = Vec::new();
        for (i, cell) in cells.into_iter().enumerate() {
            let Some(capture) = cell else { continue };
            let byte = range.start + i;
            match spans.last_mut() {
                Some(last) if last.end_byte == byte && last.capture == capture => {
                    last.end_byte = byte + 1;
                }
                _ => spans.push(Span { start_byte: byte, end_byte: byte + 1, capture }),
            }
        }
        spans
    }

    #[cfg(test)]
    fn sexp(&self) -> String {
        self.tree.root_node().to_sexp()
    }
}

/// Reads the rope in chunks rather than copying it out — the whole point of
/// incremental parsing is lost if every keystroke materialises the buffer.
fn parse(parser: &mut Parser, rope: &Rope, old: Option<&Tree>) -> Option<Tree> {
    parser.parse_with_options(
        &mut |byte, _| {
            if byte >= rope.len_bytes() {
                return &[] as &[u8];
            }
            let (chunk, chunk_start, _, _) = rope.chunk_at_byte(byte);
            &chunk.as_bytes()[byte - chunk_start..]
        },
        old,
        None,
    )
}

fn input_edit(edit: &Edit) -> InputEdit {
    InputEdit {
        start_byte: edit.start_byte,
        old_end_byte: edit.old_end_byte,
        new_end_byte: edit.new_end_byte,
        start_position: point(edit.start_point),
        old_end_position: point(edit.old_end_point),
        new_end_position: point(edit.new_end_point),
    }
}

fn point(p: crate::buffer::Point) -> TsPoint {
    TsPoint { row: p.row, column: p.col }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::Buffer;

    /// Drains the buffer's edits into the tree — the same two lines
    /// `Editor::sync_syntax` does, and the reason edits must be taken before
    /// the rope is borrowed.
    fn sync(syntax: &mut Syntax, buffer: &mut Buffer) {
        let edits = std::mem::take(&mut buffer.pending_edits);
        syntax.update(buffer.rope(), &edits);
    }

    fn rust(text: &str) -> Syntax {
        Syntax::new("rs", &Rope::from_str(text)).expect("rust grammar")
    }

    fn names(syntax: &Syntax, rope: &Rope, range: Range<usize>) -> Vec<(String, String)> {
        syntax
            .highlights(rope, range)
            .into_iter()
            .map(|s| {
                (
                    syntax.capture_name(s.capture).to_string(),
                    rope.byte_slice(s.start_byte..s.end_byte).to_string(),
                )
            })
            .collect()
    }

    #[test]
    fn an_unknown_extension_has_no_grammar() {
        assert!(Syntax::new("xyz", &Rope::from_str("hello")).is_none());
        assert!(Syntax::new("", &Rope::from_str("hello")).is_none());
    }

    #[test]
    fn keywords_and_strings_get_their_capture_names() {
        let text = "fn main() { let s = \"hi\"; }";
        let rope = Rope::from_str(text);
        let syntax = rust(text);
        let found = names(&syntax, &rope, 0..text.len());

        assert!(
            found.iter().any(|(n, t)| n.contains("keyword") && t == "fn"),
            "expected fn to be a keyword, got {found:?}"
        );
        assert!(
            found.iter().any(|(n, t)| n.contains("string") && t.contains("hi")),
            "expected the literal to be a string, got {found:?}"
        );
    }

    #[test]
    fn only_spans_inside_the_queried_range_come_back() {
        let text = "fn a() {}\nfn b() {}\nfn c() {}";
        let rope = Rope::from_str(text);
        let syntax = rust(text);

        let line2 = 10..19;
        for span in syntax.highlights(&rope, line2.clone()) {
            assert!(
                span.start_byte >= line2.start && span.end_byte <= line2.end,
                "span {span:?} escaped the queried range"
            );
        }
    }

    #[test]
    fn spans_never_overlap() {
        let text = "fn main() { let x: Vec<String> = Vec::new(); }";
        let rope = Rope::from_str(text);
        let syntax = rust(text);

        let spans = syntax.highlights(&rope, 0..text.len());
        for pair in spans.windows(2) {
            assert!(
                pair[0].end_byte <= pair[1].start_byte,
                "overlapping spans {:?} and {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    /// The invariant worth having above all others: a wrong `InputEdit` is the
    /// likeliest bug here, and it otherwise shows up as mysterious
    /// mis-highlighting long after the edit that caused it.
    #[test]
    // The final write to `at` closes the edit sequence and is not read again.
    #[allow(unused_assignments)]
    fn an_incremental_reparse_matches_a_fresh_parse() {
        let mut buffer = Buffer::empty();
        let mut at = crate::buffer::Cursor::default();
        at = buffer.insert_str(at, "fn main() {}\n");
        let mut syntax = Syntax::new("rs", buffer.rope()).unwrap();
        sync(&mut syntax, &mut buffer);

        // A spread of edits: append, insert mid-line, insert a newline, delete.
        at = crate::buffer::Cursor::at(buffer.rope().len_chars());
        at = buffer.insert_str(at, "struct S { a: u32 }\n");
        at = crate::buffer::Cursor::at(3);
        at = buffer.insert_str(at, "_renamed");
        at = crate::buffer::Cursor::at(0);
        at = buffer.insert_str(at, "// leading comment\n");
        at = crate::buffer::Cursor::at(5);
        buffer.operate(
            at,
            crate::motion::Operator::Delete,
            crate::motion::Target::Motion(crate::motion::Motion::Right),
            3,
        );

        sync(&mut syntax, &mut buffer);

        let fresh = Syntax::new("rs", buffer.rope()).unwrap();
        assert_eq!(
            syntax.sexp(),
            fresh.sexp(),
            "incremental tree diverged from a fresh parse of the same text"
        );
    }

    /// Undo replays through the same mutation primitive, so it must keep the
    /// tree correct too rather than forcing a full reparse.
    #[test]
    // The final write to `at` closes the edit sequence and is not read again.
    #[allow(unused_assignments)]
    fn an_undo_keeps_the_incremental_tree_correct() {
        let mut buffer = Buffer::empty();
        let mut at = crate::buffer::Cursor::default();
        at = buffer.insert_str(at, "fn main() {}\n");
        buffer.commit_undo(at);
        let mut syntax = Syntax::new("rs", buffer.rope()).unwrap();
        sync(&mut syntax, &mut buffer);

        at = crate::buffer::Cursor::at(buffer.rope().len_chars());
        at = buffer.insert_str(at, "struct S;\n");
        buffer.commit_undo(at);
        sync(&mut syntax, &mut buffer);

        buffer.undo(at);
        sync(&mut syntax, &mut buffer);

        let fresh = Syntax::new("rs", buffer.rope()).unwrap();
        assert_eq!(syntax.sexp(), fresh.sexp(), "tree wrong after an undo");
    }

    /// `Edit` carries byte offsets, so an edit after a multi-byte char must not
    /// hand tree-sitter a char index.
    #[test]
    // The final write to `at` closes the edit sequence and is not read again.
    #[allow(unused_assignments)]
    fn edits_after_multibyte_text_stay_correct() {
        let mut buffer = Buffer::empty();
        let mut at = crate::buffer::Cursor::default();
        at = buffer.insert_str(at, "// é comment\nfn main() {}\n");
        let mut syntax = Syntax::new("rs", buffer.rope()).unwrap();
        sync(&mut syntax, &mut buffer);

        at = crate::buffer::Cursor::at(5);
        at = buffer.insert_str(at, "ü more");
        sync(&mut syntax, &mut buffer);

        let fresh = Syntax::new("rs", buffer.rope()).unwrap();
        assert_eq!(syntax.sexp(), fresh.sexp());
    }
}
