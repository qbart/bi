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

/// The file type a name implies — `rust`, `make`, `markdown` — or `None` when
/// nothing here claims it.
///
/// This is the one place a file name becomes a language, and it answers with a
/// *name* rather than a grammar because more than the grammar is scoped by it:
/// `[filetype.go]` in a config file is, and so are the built-in defaults that
/// give a Makefile its tabs. A second table asking the same question of the
/// same string is a second answer waiting to disagree.
///
/// `file` is a file *name* — `Cargo.toml`, `CMakeLists.txt` — or a bare
/// extension, which is the same string with no dot in it. Whole names are
/// tried first, because a build file is often named rather than suffixed;
/// otherwise the text after the last dot decides. One arm per language either
/// way, so adding one stays a line.
///
/// The whole-name arm matches the name entire, never a prefix of it, which is
/// what keeps `Gemfile` on Ruby while `Gemfile.lock` — a different format
/// wearing the same first word — falls through to an extension nobody claims.
pub fn filetype(file: &str) -> Option<&'static str> {
    let named = match file {
        "CMakeLists.txt" => Some("cmake"),
        // Build files are named rather than suffixed, which is the whole
        // reason this arm exists. `*.mk` still resolves below.
        "Makefile" | "makefile" | "GNUmakefile" => Some("make"),
        "Dockerfile" => Some("dockerfile"),
        // Every dotfile the two shells read on the way in or out. None of them
        // has an extension and all of them are shell, so the whole-name arm is
        // the only thing that can reach them — and picking off `.bashrc` alone
        // is how the list ends up arbitrary.
        //
        // zsh is not bash, but the bash grammar reads all but the exotic parts
        // of a normal rc file, and there is no zsh grammar to prefer to it.
        // `.bash_aliases` is a convention rather than a file bash looks for
        // itself, which changes nothing about what is inside it.
        ".bashrc" | ".bash_profile" | ".bash_login" | ".bash_logout" | ".bash_aliases"
        | ".profile" | ".zshenv" | ".zprofile" | ".zshrc" | ".zlogin" | ".zlogout" => Some("bash"),
        // TOML, and `lock` is not a format: `yarn.lock` is bespoke,
        // `flake.lock` is JSON, `Gemfile.lock` is neither. The whole name is
        // the only honest key.
        "Cargo.lock" => Some("toml"),
        // Ruby's build files are named, extensionless Ruby — the `Makefile`
        // case again. `.rake` was already an extension while `Rakefile`, where
        // the tasks actually live, was not.
        "Gemfile" | "Rakefile" => Some("ruby"),
        _ => None,
    };
    if named.is_some() {
        return named;
    }

    Some(match file.rsplit('.').next().unwrap_or(file) {
        "rs" => "rust",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "json" => "json",
        "ini" => "ini",
        "md" | "markdown" => "markdown",
        "cmake" => "cmake",
        "mk" | "mak" => "make",
        "go" => "go",
        "c3" | "c3i" => "c3",
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" | "hh" => "cpp",
        // `.h` is ambiguous and always will be. C, because a C++ project that
        // uses it gets a grammar that reads most of the file anyway, while a C
        // project handed the C++ grammar gets nothing better.
        "c" | "h" => "c",
        "lua" => "lua",
        "sh" | "bash" | "zsh" => "bash",
        // Terraform is HCL.
        "tf" | "tfvars" | "hcl" => "hcl",
        "css" => "css",
        "slang" => "slang",
        "glsl" | "vert" | "frag" | "comp" => "glsl",
        "hlsl" => "hlsl",
        "py" | "pyi" => "python",
        "rb" | "rake" | "gemspec" => "ruby",
        "html" | "htm" => "html",
        // XML is a syntax, not a file type, and the languages already here drag
        // most of these in: `.csproj`/`.props`/`.targets` come with C#,
        // `.vcxproj` with C++, `.xaml` with a .NET UI. The grammar does not
        // care which, so each one costs a word here and costs a plain-looking
        // file if left out.
        "xml" | "xsd" | "xsl" | "xslt" | "svg" | "plist" | "csproj" | "vcxproj" | "props"
        | "targets" | "xaml" => "xml",
        "dtd" => "dtd",
        "scss" => "scss",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        // Its own name rather than TypeScript's: the grammar differs, and a
        // filetype is what someone writes `[filetype.tsx]` against.
        "tsx" => "tsx",
        "swift" => "swift",
        "java" => "java",
        // Capital `.R` is as common as lowercase, and this is given the name as
        // typed.
        "r" | "R" => "r",
        "jl" => "julia",
        "cs" => "csharp",
        "cr" => "crystal",
        "templ" => "templ",
        _ => return None,
    })
}

/// Grammar for a file type, or `None` when bi names the type but ships no
/// parser for it.
fn grammar(filetype: &str) -> Option<(Language, &'static str)> {
    Some(match filetype {
        "rust" => (tree_sitter_rust::LANGUAGE.into(), tree_sitter_rust::HIGHLIGHTS_QUERY),
        "toml" => toml(),
        "yaml" => (tree_sitter_yaml::LANGUAGE.into(), tree_sitter_yaml::HIGHLIGHTS_QUERY),
        "json" => (tree_sitter_json::LANGUAGE.into(), tree_sitter_json::HIGHLIGHTS_QUERY),
        "ini" => (tree_sitter_ini::LANGUAGE.into(), tree_sitter_ini::HIGHLIGHTS_QUERY),
        // The block grammar only. Markdown's inline syntax — emphasis, links,
        // code spans — is a *second* parser reached through an injection, and
        // injections are still deferred. Block structure is most of what you
        // look at anyway: headings, fences, list markers, quotes.
        "markdown" => (tree_sitter_md::LANGUAGE.into(), tree_sitter_md::HIGHLIGHT_QUERY_BLOCK),
        "cmake" => cmake(),
        "make" => make(),
        "dockerfile" => {
            (arborium_dockerfile::language().into(), arborium_dockerfile::HIGHLIGHTS_QUERY)
        }
        "go" => (tree_sitter_go::LANGUAGE.into(), tree_sitter_go::HIGHLIGHTS_QUERY),
        "c3" => (tree_sitter_c3::LANGUAGE.into(), tree_sitter_c3::HIGHLIGHTS_QUERY),
        "cpp" => (tree_sitter_cpp::LANGUAGE.into(), &CPP_HIGHLIGHTS),
        "c" => (tree_sitter_c::LANGUAGE.into(), tree_sitter_c::HIGHLIGHT_QUERY),
        "lua" => (tree_sitter_lua::LANGUAGE.into(), tree_sitter_lua::HIGHLIGHTS_QUERY),
        "bash" => bash(),
        // The query is bi's own — see `src/queries/hcl.scm`.
        "hcl" => (tree_sitter_hcl::LANGUAGE.into(), HCL_HIGHLIGHTS),
        "css" => (tree_sitter_css::LANGUAGE.into(), tree_sitter_css::HIGHLIGHTS_QUERY),
        // Slang and HLSL are C-family forks whose crates ship their highlight
        // queries commented out, so both borrow C's. Not C++'s: Slang rejects
        // it outright (no `auto` node) and HLSL accepts it but then matches
        // nothing at all, which is the worse failure of the two because it
        // looks like a working grammar. Both are pinned by tests below.
        "slang" => (tree_sitter_slang::LANGUAGE_SLANG.into(), tree_sitter_c::HIGHLIGHT_QUERY),
        "glsl" => (tree_sitter_glsl::LANGUAGE_GLSL.into(), tree_sitter_glsl::HIGHLIGHTS_QUERY),
        "hlsl" => (tree_sitter_hlsl::LANGUAGE_HLSL.into(), tree_sitter_c::HIGHLIGHT_QUERY),
        "python" => (tree_sitter_python::LANGUAGE.into(), tree_sitter_python::HIGHLIGHTS_QUERY),
        "ruby" => ruby(),
        "html" => (tree_sitter_html::LANGUAGE.into(), tree_sitter_html::HIGHLIGHTS_QUERY),
        "xml" => (tree_sitter_xml::LANGUAGE_XML.into(), tree_sitter_xml::XML_HIGHLIGHT_QUERY),
        // The same crate's second grammar, and its parser is compiled either
        // way — see `Cargo.toml`. A `.dtd` open beside the document it
        // constrains is the whole use.
        "dtd" => (tree_sitter_xml::LANGUAGE_DTD.into(), tree_sitter_xml::DTD_HIGHLIGHT_QUERY),
        // `language()` is a function here rather than a `LANGUAGE` const —
        // this crate predates that convention.
        "scss" => (tree_sitter_scss::language(), &SCSS_HIGHLIGHTS),
        // `HIGHLIGHT_QUERY`, singular, and a second query for JSX which needs
        // an injection to reach. `.jsx` gets the plain one until then.
        "javascript" => {
            (tree_sitter_javascript::LANGUAGE.into(), tree_sitter_javascript::HIGHLIGHT_QUERY)
        }
        "typescript" => {
            (tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), &TYPESCRIPT_HIGHLIGHTS)
        }
        "tsx" => (tree_sitter_typescript::LANGUAGE_TSX.into(), &TYPESCRIPT_HIGHLIGHTS),
        "swift" => (tree_sitter_swift::LANGUAGE.into(), tree_sitter_swift::HIGHLIGHTS_QUERY),
        "java" => (tree_sitter_java::LANGUAGE.into(), tree_sitter_java::HIGHLIGHTS_QUERY),
        "r" => (tree_sitter_r::LANGUAGE.into(), tree_sitter_r::HIGHLIGHTS_QUERY),
        // The query is vendored — see `src/queries/julia.scm`.
        "julia" => (tree_sitter_julia::LANGUAGE.into(), JULIA_HIGHLIGHTS),
        "csharp" => (tree_sitter_c_sharp::LANGUAGE.into(), tree_sitter_c_sharp::HIGHLIGHTS_QUERY),
        // The query is vendored — see `src/queries/crystal.scm`.
        "crystal" => (tree_sitter_crystal::LANGUAGE.into(), CRYSTAL_HIGHLIGHTS),
        // Go's query, then templ's own vendored half.
        "templ" => (tree_sitter_templ::LANGUAGE.into(), &TEMPL_HIGHLIGHTS),
        _ => return None,
    })
}

/// Grammar for a file, or `None` for plain text.
fn language_for(file: &str) -> Option<(Language, &'static str)> {
    grammar(filetype(file)?)
}

/// HCL's highlights, which the grammar crate does not ship. See the file.
const HCL_HIGHLIGHTS: &str = include_str!("queries/hcl.scm");

/// Julia's, which the crate ships in its package and does not export.
const JULIA_HIGHLIGHTS: &str = include_str!("queries/julia.scm");

/// Crystal's, which the grammar ships with its constant commented out.
const CRYSTAL_HIGHLIGHTS: &str = include_str!("queries/crystal.scm");

/// templ's own half, which the crate ships with its constant commented out.
const TEMPL_HIGHLIGHTS_OWN: &str = include_str!("queries/templ.scm");

/// C++'s highlights are C's, then C++'s own.
///
/// The crate's `HIGHLIGHT_QUERY` is *half* a query: upstream writes
/// `; inherits: c` at the top of it, and that line is an instruction to the
/// editor loading the file, not something the crate resolves. Alone it
/// compiles — so nothing complains — and then matches `auto`, the C++-only
/// keywords, raw strings and calls through a `qualified_identifier`. No
/// comment, no string, no number, no `if`, no `int`. A C++ file came out a
/// white wall with `auto` picked out of it.
///
/// Concatenation *is* what `; inherits:` means, and the order carries weight:
/// a tie between two patterns over the same range falls to the later one, so
/// C's `(call_expression function: (identifier) @function)` must stay after
/// its own `(identifier) @variable`, and C++'s overrides after both.
static CPP_HIGHLIGHTS: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!("{}\n{}", tree_sitter_c::HIGHLIGHT_QUERY, tree_sitter_cpp::HIGHLIGHT_QUERY)
});

/// TypeScript's highlights are JavaScript's, then TypeScript's own.
///
/// The same shape as C++: 35 lines that upstream opens `; inherits: ecma`,
/// which on its own captures types and little else — no comment, no string, no
/// keyword, no function. The concatenation *is* what the inherits line means,
/// and the order matters for the same reason it does there.
///
/// TSX shares it. The two grammars differ, the query does not.
static TYPESCRIPT_HIGHLIGHTS: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        "{}\n{}",
        tree_sitter_javascript::HIGHLIGHT_QUERY,
        tree_sitter_typescript::HIGHLIGHTS_QUERY
    )
});

/// SCSS's highlights are CSS's, then SCSS's own.
///
/// Sixty-nine lines of `@mixin`, `@include` and `@each` and nothing else — no
/// comment, no property, no number. SCSS is a superset of CSS and its grammar
/// is a fork of CSS's, so the base query compiles against it; upstream writes
/// `; inherits: css` and means exactly this.
static SCSS_HIGHLIGHTS: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!("{}\n{}", tree_sitter_css::HIGHLIGHTS_QUERY, tree_sitter_scss::HIGHLIGHTS_QUERY)
});

/// templ's highlights are Go's, then templ's own — `; inherits: go`.
///
/// templ is Go with HTML in it, so the Go half is most of the file. The HTML
/// half needs an injection and is still deferred; what the vendored query adds
/// is the templ-specific scaffolding around it.
static TEMPL_HIGHLIGHTS: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!("{}\n{}", tree_sitter_go::HIGHLIGHTS_QUERY, TEMPL_HIGHLIGHTS_OWN)
});

fn is_spell(capture: &str) -> bool {
    matches!(capture, "spell" | "nospell")
}

fn cmake() -> (Language, &'static str) {
    (tree_sitter_cmake::LANGUAGE.into(), tree_sitter_cmake::HIGHLIGHTS_QUERY)
}

fn make() -> (Language, &'static str) {
    (tree_sitter_make::LANGUAGE.into(), tree_sitter_make::HIGHLIGHTS_QUERY)
}

fn bash() -> (Language, &'static str) {
    (tree_sitter_bash::LANGUAGE.into(), tree_sitter_bash::HIGHLIGHT_QUERY)
}

fn toml() -> (Language, &'static str) {
    (tree_sitter_toml_ng::LANGUAGE.into(), tree_sitter_toml_ng::HIGHLIGHTS_QUERY)
}

fn ruby() -> (Language, &'static str) {
    (tree_sitter_ruby::LANGUAGE.into(), tree_sitter_ruby::HIGHLIGHTS_QUERY)
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
    /// How hard each capture argues for itself, indexed by capture id. See
    /// [`specificity`]. Precomputed because it is consulted while sorting
    /// every visible span, and it never changes for a given query.
    specificity: Vec<u8>,
    /// Patterns carrying a predicate tree-sitter cannot evaluate, indexed by
    /// pattern id. See [`unevaluatable`].
    unevaluatable: Vec<bool>,
}

/// Whether a pattern is guarded by a predicate tree-sitter will not run.
///
/// `#eq?`, `#match?` and `#any-of?` are evaluated by `QueryCursor::matches`
/// against the text provider. Anything else lands in `general_predicates` and
/// is simply **not applied** — the pattern then matches unconditionally, which
/// turns a narrowing rule into a blanket one.
///
/// That is not hypothetical. `#lua-match?` is Neovim's, and three queries here
/// use it: a CMake comment starting `#!/` is a `keyword.directive`, a CMake
/// argument in SHOUTING_CASE is a `constant`, and a GLSL identifier starting
/// `gl_` is a `variable.builtin`. Unevaluated, *every* CMake comment came out
/// magenta, *every* unquoted argument yellow, and *every* GLSL identifier a
/// builtin — `main` and `p` included.
///
/// Dropping the pattern is the safe direction. Each of the three is a
/// refinement of something already captured more broadly, so what is lost is a
/// shade on a rare node; what is fixed is a wrong colour on a common one.
/// Evaluating them properly means a Lua-pattern engine or a regex dependency,
/// and neither is worth three patterns.
fn unevaluatable(query: &Query, pattern: usize) -> bool {
    !query.general_predicates(pattern).is_empty()
}

/// How strong a claim a capture name makes on a node, for breaking a tie
/// between two patterns over the identical range.
///
/// Dotted segments, mostly: `string.special.key` is a narrower claim than
/// `string` and beats it. Zero for the names that are a query's *fallback*
/// rather than a claim — `@variable` is what an identifier is called when
/// nothing more interesting was said about it, and `@none` explicitly asks for
/// no colour at all.
///
/// Without the exception, the tie falls through to pattern order, and the
/// queries do not agree on one. C writes `(identifier) @variable` first and its
/// `@function` patterns last, so later-wins is right there. Go writes them the
/// other way round — `(function_declaration name: (identifier) @function)` on
/// line 17, the blanket `(identifier) @variable` on line 26 — because that
/// query is written for `tree-sitter-highlight`, where the *first* pattern
/// wins. Both spellings are correct upstream and they are exact opposites, so
/// no rule reading pattern order alone can serve them both. `func main()` in Go
/// came out a variable, which is to say uncoloured, next to a C file where the
/// same construct came out a function.
fn specificity(name: &str) -> u8 {
    if matches!(name, "variable" | "none") {
        return 0;
    }
    name.matches('.').count() as u8 + 1
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
        let specificity = query.capture_names().iter().map(|n| specificity(n)).collect();
        let unevaluatable =
            (0..query.pattern_count()).map(|p| unevaluatable(&query, p)).collect::<Vec<_>>();
        Some(Self { parser, tree, query, specificity, unevaluatable })
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
            // A predicate tree-sitter did not run is a guard that did not
            // hold, so the whole pattern goes rather than firing on every
            // node it can reach.
            if self.unevaluatable[m.pattern_index] {
                continue;
            }
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
        // the stronger claim wins (see `specificity`), then the later pattern.
        // A JSON key is both `string.special.key` and `string`, a YAML key is
        // both `property` and `string`, and a Go function name is both
        // `function` and the blanket `variable` — and the queries disagree
        // about which order to write each pair in. Without this, keys take the
        // colour of ordinary string values and a config file reads as one
        // green wall, and every Go declaration goes uncoloured.
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
    const KNOWN: &[&str] = &[
        "rs",
        "toml",
        "yaml",
        "yml",
        "json",
        "ini",
        "md",
        "markdown",
        "cmake",
        "CMakeLists.txt",
        "go",
        "c3",
        "c3i",
        "cpp",
        "cc",
        "cxx",
        "hpp",
        "hxx",
        "hh",
        "c",
        "h",
        "lua",
        "sh",
        "bash",
        "zsh",
        ".bashrc",
        ".bash_profile",
        ".bash_login",
        ".bash_logout",
        ".bash_aliases",
        ".profile",
        ".zshenv",
        ".zprofile",
        ".zshrc",
        ".zlogin",
        ".zlogout",
        "Dockerfile",
        "tf",
        "tfvars",
        "hcl",
        "css",
        "slang",
        "glsl",
        "vert",
        "frag",
        "comp",
        "hlsl",
        "py",
        "pyi",
        "Makefile",
        "makefile",
        "GNUmakefile",
        "mk",
        "mak",
        "rb",
        "rake",
        "gemspec",
        "Gemfile",
        "Rakefile",
        "Cargo.lock",
        "html",
        "htm",
        "xml",
        "xsd",
        "xsl",
        "xslt",
        "svg",
        "plist",
        "csproj",
        "vcxproj",
        "props",
        "targets",
        "xaml",
        "dtd",
        "scss",
        "js",
        "jsx",
        "mjs",
        "cjs",
        "ts",
        "mts",
        "cts",
        "tsx",
        "swift",
        "java",
        "r",
        "R",
        "jl",
        "cs",
        "templ",
        "cr",
    ];

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

    /// A grammar whose query compiles but matches *nothing* is the worst
    /// failure available here: it looks installed and renders plain. HLSL did
    /// exactly that with the C++ query, so every borrowed or hand-written
    /// query gets a snippet that must come back with captures in it.
    #[test]
    fn the_queries_bi_did_not_get_from_a_crate_produce_captures() {
        // Terraform, whose query is bi's own — `src/queries/hcl.scm`.
        let tf =
            captures("tf", "# note\nresource \"aws_instance\" \"web\" {\n  ami = var.image\n}\n");
        covers(&tf, "comment", "# note");
        covers(&tf, "keyword", "resource");
        covers(&tf, "type", "aws_instance");
        covers(&tf, "property", "ami");
        covers(&tf, "variable", "var");

        // Slang and HLSL, borrowing C's.
        for shader in ["slang", "hlsl"] {
            let found = captures(shader, "// note\nfloat4 main(float2 uv) { return 0; }\n");
            covers(&found, "comment", "// note");
            covers(&found, "type", "float4");
            covers(&found, "function", "main");
            assert!(found.len() > 3, "{shader} matched almost nothing: {found:?}");
        }
    }

    /// A crate that exports `HIGHLIGHTS_QUERY` is not evidence the query is
    /// whole. C++'s is a `; inherits: c` delta and compiled happily on its own
    /// while matching `auto` and almost nothing else, so every language gets a
    /// snippet whose comment, keyword and literal must all come back captured.
    #[test]
    fn every_language_captures_a_comment_a_keyword_and_a_literal() {
        // (one key per grammar, snippet, comment, keyword, literal)
        let cases: &[(&str, &str, &str, &str, &str)] = &[
            ("rs", "// note\nfn f() { let s = \"hi\"; }\n", "// note", "fn", "\"hi\""),
            ("go", "// note\nfunc f() { s := \"hi\" }\n", "// note", "func", "\"hi\""),
            ("py", "# note\ndef f():\n    return \"hi\"\n", "# note", "def", "\"hi\""),
            ("lua", "-- note\nlocal s = \"hi\"\n", "-- note", "local", "\"hi\""),
            ("sh", "# note\nif true; then s=\"hi\"; fi\n", "# note", "if", "\"hi\""),
            (
                "c",
                "// note\nint f(void) { char *s = \"hi\"; return 0; }\n",
                "// note",
                "return",
                "\"hi\"",
            ),
            // The regression: C++ alone captured `auto` and nothing else.
            (
                "cpp",
                "// note\nint f() { const char *s = \"hi\"; return 0; }\n",
                "// note",
                "return",
                "\"hi\"",
            ),
            (
                "c3",
                "// note\nfn int f() { String s = \"hi\"; return 0; }\n",
                "// note",
                "fn",
                "\"hi\"",
            ),
            // GLSL's query captures no boolean, so the literal here is a
            // number — the assertion is that literals reach the frontend at
            // all, not that a particular spelling of one does.
            ("glsl", "// note\nvoid main() { if (1) {} }\n", "// note", "if", "1"),
            ("hlsl", "// note\nfloat f() { if (1) {} return 0; }\n", "// note", "return", "0"),
            ("slang", "// note\nfloat f() { if (1) {} return 0; }\n", "// note", "return", "0"),
            // CSS leaves a bare `red` uncaptured — a `plain_value` is not a
            // literal to that query — so the number carries the third slot.
            ("css", "/* note */\na { color: red; margin: 0 }\n", "/* note */", "color", "0"),
            ("toml", "# note\n[s]\nk = \"hi\"\n", "# note", "k", "\"hi\""),
            ("yaml", "# note\nk: \"hi\"\n", "# note", "k", "\"hi\""),
            ("cmake", "# note\nif(A)\n  project(bi)\nendif()\n", "# note", "if", "project"),
            (
                "tf",
                "# note\nresource \"a\" \"b\" {\n  k = \"hi\"\n}\n",
                "# note",
                "resource",
                "\"hi\"",
            ),
            // A Makefile has no literals to speak of; the variable reference is
            // the nearest thing, and it arrives split into four captures.
            ("Makefile", "# note\nall: dep\n\tcp a $(B)\n", "# note", "all", "B"),
            ("rb", "# note\ndef f\n  s = \"hi\"\nend\n", "# note", "def", "\"hi\""),
            // HTML captures the attribute value without its quotes.
            ("html", "<!-- note -->\n<a href=\"x\">t</a>\n", "<!-- note -->", "a", "x"),
            ("scss", "// note\n.a { color: red; width: 0 }\n", "// note", "color", "0"),
            ("js", "// note\nfunction f() { return \"hi\"; }\n", "// note", "return", "\"hi\""),
            // The regression this file already knows by heart: TypeScript's
            // own query is thirty-five lines of types and nothing else.
            (
                "ts",
                "// note\nfunction f(): string { return \"hi\"; }\n",
                "// note",
                "return",
                "\"hi\"",
            ),
            (
                "tsx",
                "// note\nfunction f(): string { return \"hi\"; }\n",
                "// note",
                "return",
                "\"hi\"",
            ),
            (
                "swift",
                "// note\nfunc f() -> String { return \"hi\" }\n",
                "// note",
                "func",
                "\"hi\"",
            ),
            ("java", "// note\nclass C { int f() { return 1; } }\n", "// note", "class", "1"),
            ("r", "# note\nf <- function(x) { return(\"hi\") }\n", "# note", "function", "\"hi\""),
            (
                "jl",
                "# note\nfunction f(x)\n    return \"hi\"\nend\n",
                "# note",
                "function",
                "\"hi\"",
            ),
            ("cs", "// note\nclass C { int F() { return 1; } }\n", "// note", "class", "1"),
            (
                "templ",
                "// note\npackage a\n\nfunc f() string { return \"hi\" }\n",
                "// note",
                "func",
                "\"hi\"",
            ),
            ("cr", "# note\ndef f\n  s = \"hi\"\nend\n", "# note", "def", "\"hi\""),
            // XML's keyword and its literal both live in the declaration —
            // `<note>` is a tag, not a keyword, and the text between tags is
            // `@markup` rather than a string.
            (
                "xml",
                "<?xml version=\"1.0\"?>\n<!-- note -->\n<note to=\"x\">hi</note>\n",
                "<!-- note -->",
                "xml",
                "1.0",
            ),
            (
                "dtd",
                "<?xml version=\"1.0\"?>\n<!-- note -->\n<!ELEMENT note (#PCDATA)>\n",
                "<!-- note -->",
                "ELEMENT",
                "1.0",
            ),
        ];

        for (file, text, comment, keyword, literal) in cases {
            let found = captures(file, text);
            for wanted in [comment, keyword, literal] {
                assert!(
                    found.iter().any(|(_, t)| t.trim() == *wanted),
                    "{file}: nothing captured {wanted:?} — got {found:?}"
                );
            }
            covers(&found, "comment", comment);
        }

        // Three that cannot fill all three slots, asserted on what they do
        // have rather than left untested: JSON has no comment, INI captures
        // no value, and Dockerfile no literal.
        let json = captures("json", "{\"a\": 1}");
        covers(&json, "string.special.key", "\"a\"");
        covers(&json, "number", "1");

        let ini = captures("ini", "; note\n[s]\nk = hi\n");
        covers(&ini, "comment", "; note");
        covers(&ini, "property", "k");

        let docker = captures("Dockerfile", "# note\nFROM alpine:3.19\n");
        covers(&docker, "comment", "# note");
        covers(&docker, "keyword", "FROM");

        // Markdown is neither: it has no comment, keyword or literal at all.
        // `markdown_highlights_its_block_structure` is its equivalent.
    }

    /// C++'s crate query is the `; inherits: c` half. On its own it captured
    /// `auto` and nothing else, so a C++ file rendered as a white wall — the
    /// C half has to be in front of it, and its own overrides still on top.
    #[test]
    fn cpp_gets_cs_query_as_well_as_its_own() {
        let found = captures(
            "cpp",
            "// note\n#include <a.hpp>\nint f() {\n  auto s = \"hi\";\n  if (s) return 1;\n}\n",
        );

        // From C's half, none of which C++'s ships.
        covers(&found, "comment", "// note");
        covers(&found, "keyword", "#include");
        covers(&found, "string", "<a.hpp>");
        covers(&found, "type", "int");
        covers(&found, "string", "\"hi\"");
        covers(&found, "keyword", "if");
        covers(&found, "keyword", "return");
        covers(&found, "number", "1");
        covers(&found, "function", "f");

        // From C++'s own half, which must still win where the two overlap.
        covers(&found, "type", "auto");

        // C's `(identifier) @variable` is the first pattern in the combined
        // query and matches every identifier there is. Concatenating in the
        // other order would let it beat the function it contains, since a tie
        // over one range falls to the later pattern.
        assert!(
            !found.iter().any(|(n, t)| n == "variable" && t == "f"),
            "the blanket @variable outran @function: {found:?}"
        );
    }

    /// `@variable` is what a query calls an identifier when it had nothing
    /// better to say, so it must never outrank something that did. Go writes
    /// the blanket after its `@function` patterns — correct for
    /// `tree-sitter-highlight`, where the first pattern wins — and C writes it
    /// before, correct for last-wins. Pattern order alone cannot serve both.
    #[test]
    fn a_blanket_variable_never_outranks_a_real_capture() {
        let go = captures("go", "func main() {\n\ttype T int\n}\n");
        covers(&go, "function", "main");
        assert!(
            !go.iter().any(|(n, t)| n == "variable" && t == "main"),
            "the blanket @variable swallowed a declaration: {go:?}"
        );

        // The same tie in the order C writes it, which already worked and has
        // to keep working.
        let c = captures("c", "int f(void) { return g(); }\n");
        covers(&c, "function", "f");
        covers(&c, "function", "g");
    }

    /// A predicate tree-sitter does not evaluate is a guard that does not
    /// hold, and the pattern then fires on everything it can reach. `#lua-match?`
    /// is Neovim's and three queries here use it, so all three used to paint
    /// far more than they meant to.
    #[test]
    fn a_predicate_tree_sitter_cannot_run_takes_its_pattern_with_it() {
        // `((line_comment) @keyword.directive (#lua-match? … "^#!/"))` — every
        // CMake comment came back a directive, so comments rendered magenta.
        let cmake = captures("CMakeLists.txt", "# note\nproject(bi)\n");
        covers(&cmake, "comment", "# note");
        assert!(
            !cmake.iter().any(|(n, _)| n.starts_with("keyword.directive")),
            "an unrun #lua-match? still painted: {cmake:?}"
        );

        // `((unquoted_argument) @constant (#lua-match? … "^[%u@][%u%d_]+$"))`
        // — `bi` is not SHOUTING_CASE and is not a constant.
        assert!(
            !cmake.iter().any(|(n, t)| n.starts_with("constant") && t == "bi"),
            "a lowercase argument came back a constant: {cmake:?}"
        );

        // `((identifier) @variable.builtin (#lua-match? … "^gl_"))` — this one
        // caught *every* GLSL identifier, `main` included.
        let glsl = captures("glsl", "void main() { float x = 1.0; }\n");
        assert!(
            !glsl.iter().any(|(n, t)| n.starts_with("variable.builtin") && t == "main"),
            "every glsl identifier is still a builtin: {glsl:?}"
        );
    }

    #[test]
    fn the_added_languages_highlight_their_own_shapes() {
        let go = captures("go", "package main\n\nfunc f() int { return 1 }\n");
        covers(&go, "keyword", "func");

        let py = captures("py", "def f(x):\n    return \"s\"\n");
        covers(&py, "keyword", "def");
        covers(&py, "function", "f");

        let c = captures("c", "int main(void) { return 0; }\n");
        covers(&c, "type", "int");

        let lua = captures("lua", "local x = 1\n");
        covers(&lua, "keyword", "local");

        let sh = captures(".bashrc", "# note\nexport PATH=/bin\n");
        covers(&sh, "comment", "# note");

        let docker = captures("Dockerfile", "FROM alpine:3.19\n");
        covers(&docker, "keyword", "FROM");

        let c3 = captures("c3", "fn int main() { return 0; }\n");
        covers(&c3, "keyword", "fn");
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

    /// A grammar can be a stub, and it says so with neither an error nor a
    /// missing symbol — the crates.io `tree-sitter-crystal` exports a
    /// `LANGUAGE`, compiles, and puts twelve `ERROR` nodes through this. The
    /// check is parsing real code and counting the wreckage, and it is what
    /// any future grammar should have to survive.
    #[test]
    fn crystal_parses_real_crystal_rather_than_a_subset_of_it() {
        let real = concat!(
            "require \"http/client\"\n\n",
            "module Greeter\n",
            "  VERSION = \"0.1.0\"\n\n",
            "  struct Point\n",
            "    property x : Int32\n",
            "  end\n\n",
            "  def self.run(names : Array(String)) : Hash(String, Int32)\n",
            "    counts = {} of String => Int32\n",
            "    names.each do |name|\n",
            "      counts[name] = name.size\n",
            "      puts \"hello, #{name}!\"\n",
            "    end\n",
            "    counts\n",
            "  end\n",
            "end\n",
        );
        let rope = Rope::from_str(real);
        let syntax = Syntax::new("cr", &rope).expect("crystal grammar");
        let sexp = syntax.sexp();
        assert!(
            !sexp.contains("ERROR") && !sexp.contains("MISSING"),
            "the crystal grammar could not read ordinary crystal: {sexp}"
        );

        // And the vendored query has to actually fire on it.
        let found = names(&syntax, &rope, 0..real.len());
        covers(&found, "keyword", "module");
        covers(&found, "keyword", "def");
        covers(&found, "string", "\"0.1.0\"");
        covers(&found, "type", "Int32");
    }

    /// An XML document is almost entirely tags, so `@tag` is the capture that
    /// decides whether the grammar was worth adding. The three parts of a tag
    /// have to come back as three different names, or the file reads as one
    /// colour — the failure this file already knows from JSON keys.
    #[test]
    fn xml_separates_a_tag_from_its_attribute_and_its_value() {
        let found = captures("xml", "<note to=\"x\">hi</note>\n");
        covers(&found, "tag", "note");
        covers(&found, "property", "to");
        covers(&found, "string", "x");

        // Both the open and the close tag, since a closing name is a separate
        // node and a query can easily capture only the first.
        assert_eq!(
            found.iter().filter(|(n, t)| n == "tag" && t == "note").count(),
            2,
            "only one of the two tag names came back: {found:?}"
        );
    }

    /// A file whose *name* carries the language, for the three cases where the
    /// suffix could never have found it. `Cargo.lock` is the one that bites:
    /// `lock` is not a format — `yarn.lock` is bespoke and `flake.lock` is
    /// JSON — so the key has to be the whole name.
    #[test]
    fn a_named_file_reaches_a_grammar_its_suffix_could_not() {
        let lock = captures("Cargo.lock", "[[package]]\nname = \"bi\"\n");
        covers(&lock, "type", "name");
        covers(&lock, "string", "\"bi\"");

        for name in ["Gemfile", "Rakefile"] {
            let found = captures(name, "# note\nsource \"https://rubygems.org\"\n");
            covers(&found, "comment", "# note");
            covers(&found, "string", "\"https://rubygems.org\"");
        }

        // And the whole name is matched whole. `Gemfile.lock` starts with a key
        // in the table and is not that format — nor any other one here.
        assert!(Syntax::new("Gemfile.lock", &Rope::from_str("GEM\n")).is_none());
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
