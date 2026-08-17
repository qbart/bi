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

/// Grammar for a file, or `None` for plain text.
///
/// `file` is a file *name* — `Cargo.toml`, `CMakeLists.txt` — or a bare
/// extension, which is the same string with no dot in it. Whole names are
/// tried first, because a build file is often named rather than suffixed;
/// otherwise the text after the last dot decides. One arm per language either
/// way, so adding one stays a line.
fn language_for(file: &str) -> Option<(Language, &'static str)> {
    if file == "CMakeLists.txt" {
        return Some(cmake());
    }
    match file.rsplit('.').next().unwrap_or(file) {
        "rs" => Some((tree_sitter_rust::LANGUAGE.into(), tree_sitter_rust::HIGHLIGHTS_QUERY)),
        "toml" => {
            Some((tree_sitter_toml_ng::LANGUAGE.into(), tree_sitter_toml_ng::HIGHLIGHTS_QUERY))
        }
        "yaml" | "yml" => {
            Some((tree_sitter_yaml::LANGUAGE.into(), tree_sitter_yaml::HIGHLIGHTS_QUERY))
        }
        "json" => Some((tree_sitter_json::LANGUAGE.into(), tree_sitter_json::HIGHLIGHTS_QUERY)),
        "ini" => Some((tree_sitter_ini::LANGUAGE.into(), tree_sitter_ini::HIGHLIGHTS_QUERY)),
        // The block grammar only. Markdown's inline syntax — emphasis, links,
        // code spans — is a *second* parser reached through an injection, and
        // injections are still deferred. Block structure is most of what you
        // look at anyway: headings, fences, list markers, quotes.
        "md" | "markdown" => {
            Some((tree_sitter_md::LANGUAGE.into(), tree_sitter_md::HIGHLIGHT_QUERY_BLOCK))
        }
        "cmake" => Some(cmake()),
        _ => None,
    }
}

fn is_spell(capture: &str) -> bool {
    matches!(capture, "spell" | "nospell")
}

fn cmake() -> (Language, &'static str) {
    (tree_sitter_cmake::LANGUAGE.into(), tree_sitter_cmake::HIGHLIGHTS_QUERY)
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
    /// Dotted segments per capture, indexed by capture id. Precomputed
    /// because it is consulted while sorting every visible span, and it
    /// never changes for a given query.
    specificity: Vec<u8>,
}

impl Syntax {
    /// Parses `rope` for the grammar matching `file` — a file name, or a bare
    /// extension. `None` when no grammar is known: an unrecognised file is
    /// plain text, never an error.
    pub fn new(file: &str, rope: &Rope) -> Option<Self> {
        let (language, highlights) = language_for(file)?;
        let mut parser = Parser::new();
        parser.set_language(&language).ok()?;
        let query = Query::new(&language, highlights).ok()?;
        let tree = parse(&mut parser, rope, None)?;
        let specificity =
            query.capture_names().iter().map(|n| n.matches('.').count() as u8 + 1).collect();
        Some(Self { parser, tree, query, specificity })
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

        let mut raw: Vec<(usize, usize, u32, usize)> = Vec::new();
        let mut matches = cursor.matches(&self.query, self.tree.root_node(), RopeProvider(rope));
        while let Some(m) = matches.next() {
            for capture in m.captures {
                // `@spell` / `@nospell` mark where a spellchecker should look;
                // they say nothing about colour. Several grammars hang one on
                // the same node as `@comment` — INI and CMake both do — so
                // letting them through would leave comments competing with a
                // capture that no theme has an entry for.
                if is_spell(self.capture_name(capture.index)) {
                    continue;
                }
                let node = capture.node.byte_range();
                raw.push((node.start, node.end, capture.index, m.pattern_index));
            }
        }

        // Widest first, so a nested capture overwrites the one containing it —
        // the innermost match is the specific one.
        //
        // Two patterns capturing the *same* range is not nesting, and the
        // order they arrive in is not meaningful, so it is broken explicitly:
        // the more dotted name wins, then the later pattern. A JSON key is
        // both `string.special.key` and `string`, a YAML key is both
        // `property` and `string`, and the queries disagree about which
        // order to write them in — without this, keys take the colour of
        // ordinary string values and a config file reads as one green wall.
        raw.sort_by_key(|(start, end, capture, pattern)| {
            (std::cmp::Reverse(end - start), self.specificity[*capture as usize], *pattern)
        });

        let mut cells: Vec<Option<u32>> = vec![None; range.len()];
        for (start, end, capture, _) in raw {
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
    /// `Editor::settle` does, and the reason edits must be taken before
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

    /// Every name the table claims. A grammar whose query fails to compile
    /// against the tree-sitter version in `Cargo.toml` degrades *silently* to
    /// plain text — `Query::new(..).ok()?` — so nothing but a test that asks
    /// for each one will notice.
    const KNOWN: &[&str] =
        &["rs", "toml", "yaml", "yml", "json", "ini", "md", "markdown", "cmake", "CMakeLists.txt"];

    /// The capture names for `text`, in order, with the text they cover.
    fn captures(file: &str, text: &str) -> Vec<(String, String)> {
        let rope = Rope::from_str(text);
        let syntax = Syntax::new(file, &rope).unwrap_or_else(|| panic!("no grammar for {file}"));
        names(&syntax, &rope, 0..text.len())
    }

    /// Asserts some capture whose name starts with `capture` covers `text`.
    fn covers(found: &[(String, String)], capture: &str, text: &str) {
        assert!(
            found.iter().any(|(n, t)| n.starts_with(capture) && t.trim() == text),
            "expected {text:?} to be a {capture}, got {found:?}"
        );
    }

    #[test]
    fn every_grammar_in_the_table_parses_and_queries() {
        for file in KNOWN {
            assert!(Syntax::new(file, &Rope::from_str("")).is_some(), "no grammar for {file}");
        }
    }

    #[test]
    fn cmake_is_found_by_file_name_as_well_as_extension() {
        // The point of keying on the name: nobody writes `*.cmake` nearly as
        // often as they write this one file.
        let found = captures("CMakeLists.txt", "project(bi)\n");
        covers(&found, "function", "project");
    }

    #[test]
    fn toml_yaml_json_and_ini_highlight_their_keys_and_values() {
        let toml = captures("toml", "[server]\nport = 8080\nname = \"bi\"\n");
        covers(&toml, "type", "port");
        covers(&toml, "number", "8080");
        covers(&toml, "string", "\"bi\"");

        let yaml = captures("yaml", "key: value\ncount: 1\n");
        covers(&yaml, "property", "key");
        covers(&yaml, "string", "value");
        covers(&yaml, "number", "1");

        let json = captures("json", "{\"a\": 1, \"b\": \"two\"}");
        covers(&json, "string.special.key", "\"a\"");
        covers(&json, "number", "1");

        let ini = captures("ini", "[sec]\nkey = val\n");
        covers(&ini, "type", "sec");
        covers(&ini, "property", "key");
    }

    #[test]
    fn markdown_highlights_its_block_structure() {
        // Block grammar only, so a heading and a fence are captured but the
        // `**bold**` inside the paragraph is not — that needs the inline
        // grammar, and an injection to reach it.
        let found = captures("md", "# Title\n\nsome **bold** text\n");
        covers(&found, "text.title", "Title");
        assert!(
            !found.iter().any(|(n, t)| t.contains("bold") && n.starts_with("text.emphasis")),
            "inline emphasis is not expected before injections land: {found:?}"
        );
    }

    #[test]
    fn cmake_highlights_commands_and_control_flow() {
        let found = captures("CMakeLists.txt", "if(A)\n  project(bi)\nendif()\n");
        covers(&found, "keyword", "if");
        covers(&found, "function", "project");
    }

    /// Two patterns over the byte-identical range is not nesting, and the
    /// order tree-sitter yields them in is not meaningful. A key that reads
    /// as an ordinary string value is the visible symptom.
    #[test]
    fn a_key_captured_twice_keeps_the_more_specific_name() {
        let json = captures("json", "{\"a\": \"b\"}");
        covers(&json, "string.special.key", "\"a\"");
        assert!(
            json.iter().any(|(n, t)| n == "string" && t == "\"b\""),
            "the value should stay a plain string, got {json:?}"
        );

        // YAML writes the two patterns in the opposite order to JSON, so a
        // rule based on pattern order alone would fix one and break the other.
        let yaml = captures("yaml", "key: value\n");
        covers(&yaml, "property", "key");
        assert!(
            yaml.iter().any(|(n, t)| n == "string" && t == "value"),
            "the value should stay a plain string, got {yaml:?}"
        );
    }

    /// `@spell` is a spellchecker hint, and INI hangs one on the same node as
    /// `@comment`. If it competes, comments come out unstyled.
    #[test]
    fn a_spellcheck_marker_never_wins_a_capture() {
        let found = captures("ini", "; a note\nkey = val\n");
        covers(&found, "comment", "; a note");
        assert!(
            !found.iter().any(|(n, _)| n == "spell" || n == "nospell"),
            "a spell marker reached the frontend: {found:?}"
        );
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
        buffer.commit_undo(vec![(at.at, at.at)], vec![(at.at, at.at)]);
        let mut syntax = Syntax::new("rs", buffer.rope()).unwrap();
        sync(&mut syntax, &mut buffer);

        at = crate::buffer::Cursor::at(buffer.rope().len_chars());
        at = buffer.insert_str(at, "struct S;\n");
        buffer.commit_undo(vec![(at.at, at.at)], vec![(at.at, at.at)]);
        sync(&mut syntax, &mut buffer);

        buffer.undo(vec![(at.at, at.at)], vec![(at.at, at.at)]);
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
