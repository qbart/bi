//! What an indent is, and how wide a line is when it is drawn.
//!
//! The width of a tab decides where the cursor is drawn, where an indent guide
//! goes, and how far `>` moves a line. Two of those are editor semantics, so
//! the number lives here rather than in a frontend that would have to guess it
//! — it used to be a `const` in `src/tui/render.rs`, which is exactly the
//! guess this module exists to stop.
//!
//! Nothing here touches a rope: it takes a `&str` and a width and answers
//! questions about columns. The edits are [`crate::buffer::Buffer`]'s.
//!
//! See `docs/specs/indent.md`.

/// The settings that decide what indentation looks like.
///
/// Copied out of [`crate::config::Options`] and handed down, rather than being
/// reached for: `Buffer` knows nothing about config, and when options become
/// per-file this is the value that will be resolved per buffer instead of per
/// session — the call sites will not have to change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Indent {
    pub tab_width: usize,
    pub expandtab: bool,
    /// 0 means "whatever `tab_width` says" — see [`Indent::step`].
    pub shiftwidth: usize,
    pub autoindent: bool,
}

impl Default for Indent {
    fn default() -> Self {
        Self { tab_width: 4, expandtab: true, shiftwidth: 0, autoindent: true }
    }
}

impl Indent {
    /// How far one `>` moves, in columns.
    ///
    /// Vim's `shiftwidth = 0` rule: almost nobody wants the shift to differ
    /// from the tab stop, so one knob controls both until someone deliberately
    /// wants two. Never returns 0 — a step of nothing would make `>` a no-op
    /// and `Tab` an infinite loop.
    pub fn step(&self) -> usize {
        match self.shiftwidth {
            0 => self.tab_width.max(1),
            n => n,
        }
    }

    /// `width` columns of indentation, written the way the options ask for.
    ///
    /// The whole of the tabs-versus-spaces policy, in the one place `>`, `Tab`
    /// and autoindent all reach for it, so they cannot disagree.
    ///
    /// The remainder is spaces because there is no such thing as most of a
    /// tab. It only appears when the step does not divide the tab width, which
    /// someone has to ask for; when they do, the file still lines up on
    /// screen, which is the only promise indentation makes.
    pub fn render(&self, width: usize) -> String {
        if self.expandtab {
            return " ".repeat(width);
        }
        let tabs = width / self.tab_width.max(1);
        let spaces = width % self.tab_width.max(1);
        let mut out = String::with_capacity(tabs + spaces);
        out.extend(std::iter::repeat_n('\t', tabs));
        out.extend(std::iter::repeat_n(' ', spaces));
        out
    }
}

/// Screen column of char offset `char_col` within `line`.
pub fn display_col(line: &str, char_col: usize, tab_width: usize) -> usize {
    let tab_width = tab_width.max(1);
    let mut col = 0;
    for ch in line.chars().take(char_col) {
        col += if ch == '\t' { tab_width - (col % tab_width) } else { 1 };
    }
    col
}

/// How many columns `text` occupies, starting from column zero.
pub fn width_of(text: &str, tab_width: usize) -> usize {
    display_col(text, text.chars().count(), tab_width)
}

/// Expands tabs for display.
///
/// Width is counted in chars, so wide (CJK) and combining chars will be off.
/// Fixing that means a `unicode-width` dependency and a real grapheme walk —
/// worth doing before this is usable on non-Latin text.
pub fn expand_tabs(line: &str, tab_width: usize) -> String {
    if !line.contains('\t') {
        return line.to_string();
    }
    let tab_width = tab_width.max(1);
    let mut out = String::with_capacity(line.len());
    let mut col = 0;
    for ch in line.chars() {
        if ch == '\t' {
            let n = tab_width - (col % tab_width);
            out.extend(std::iter::repeat_n(' ', n));
            col += n;
        } else {
            out.push(ch);
            col += 1;
        }
    }
    out
}

/// The leading whitespace of `line`, as it is written.
///
/// Returned as a slice rather than a width because autoindent copies the
/// characters: a tab-indented file stays tab-indented even under `expandtab`,
/// which is what makes a new line match its neighbour.
pub fn leading(line: &str) -> &str {
    let end = line.find(|c: char| c != ' ' && c != '\t').unwrap_or(line.len());
    &line[..end]
}

/// `line`'s leading whitespace, rewritten the way `indent` asks for it —
/// `None` when it is already written that way.
///
/// The whole of `:retab`, and it is three lines because [`Indent::render`] is
/// already the tabs-versus-spaces policy: measure the indent in columns, then
/// ask for that many columns back. A file converted this way is the same width
/// on screen as it was, which is the only promise worth making — the point is
/// to change the characters, not the layout.
///
/// **Leading whitespace only.** A tab inside a string literal is content, and
/// an editor that rewrites it has corrupted the file to satisfy a setting
/// about indentation. That is the difference between this and vim's `:retab`,
/// which walks every whitespace run on the line.
///
/// **A line with nothing but whitespace on it is left alone.** It has no
/// indentation to convert — it has trailing whitespace, and removing that is
/// `trim_trailing`'s job (`docs/specs/trim.md`). Two features rewriting the
/// same characters to different ends is how they come to disagree.
pub fn retab(line: &str, indent: &Indent) -> Option<String> {
    let was = leading(line);
    if was.len() == line.len() {
        return None;
    }
    let now = indent.render(width_of(was, indent.tab_width));
    (now != was).then_some(now)
}

/// The new indentation for the rows from `first` to the end of `lines` —
/// `=`'s answer, one rendered indent per row.
///
/// What the structure wants is bracket depth: one step per `{`, `[` or `(`
/// left open on the lines above, minus the closers a line itself leads with.
/// The rows before `first` are context — walked, never touched — which is
/// how `==` on one line knows where it stands.
///
/// The counting is textual until it is syntactic: a bracket inside a string
/// counts, and a language whose blocks are not bracketed gets flattened.
/// That is vim's own `=` baseline, and tree-sitter indent queries replace
/// this function without changing its shape — the same seam `matches_in`
/// leaves for regex. See `docs/specs/indent.md`.
///
/// A blank line's indent is the empty string: pushing whitespace onto a line
/// with nothing on it buys nothing and fills the diff.
pub fn reindent(lines: &[String], first: usize, indent: &Indent) -> Vec<String> {
    let mut depth: usize = 0;
    for line in &lines[..first.min(lines.len())] {
        depth = depth_after(line, depth);
    }
    let mut out = Vec::new();
    for line in &lines[first.min(lines.len())..] {
        let text = line.trim_start();
        if text.is_empty() {
            out.push(String::new());
        } else {
            let effective = depth.saturating_sub(leading_closers(text));
            out.push(indent.render(effective * indent.step()));
        }
        depth = depth_after(line, depth);
    }
    out
}

/// The bracket depth in force after `line`, clamped at zero — an over-closed
/// file cannot owe indentation.
fn depth_after(line: &str, mut depth: usize) -> usize {
    for c in line.chars() {
        match c {
            '{' | '[' | '(' => depth += 1,
            '}' | ']' | ')' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    depth
}

/// How many closers `text` (already trimmed) leads with — the `}` that sits
/// with the line that opened it, and the `})` that closes two at once.
fn leading_closers(text: &str) -> usize {
    text.chars().take_while(|c| matches!(c, '}' | ']' | ')')).count()
}

/// The columns a line indented `width` gets vertical guides at: 0, one step
/// in, two steps in, up to but not including the text itself.
///
/// Column 0 included, because the outermost level is a level. `width` itself
/// excluded, because a guide there would sit on the first character of the
/// line rather than in the whitespace before it.
pub fn guide_columns(width: usize, step: usize) -> impl Iterator<Item = usize> {
    (0..width).step_by(step.max(1))
}

/// Whether `line` has nothing on it but whitespace. An empty line qualifies.
pub fn is_blank(line: &str) -> bool {
    line.chars().all(|c| c == ' ' || c == '\t' || c == '\r' || c == '\n')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spaces() -> Indent {
        Indent::default()
    }

    fn tabs() -> Indent {
        Indent { expandtab: false, ..Indent::default() }
    }

    fn reindented(text: &[&str], first: usize) -> Vec<String> {
        let lines: Vec<String> = text.iter().map(|s| s.to_string()).collect();
        reindent(&lines, first, &spaces())
    }

    #[test]
    fn reindent_puts_a_line_at_one_step_per_open_bracket() {
        assert_eq!(
            reindented(&["fn f() {", "let x = 1;", "if y {", "z();", "}", "}"], 0),
            ["", "    ", "    ", "        ", "    ", ""],
            "and a closer sits with the line that opened it"
        );
    }

    /// The rows before `first` are context: `==` on one line still knows
    /// where it stands.
    #[test]
    fn reindent_reads_the_lines_above_without_touching_them() {
        assert_eq!(reindented(&["fn f() {", "        x();"], 1), ["    "]);
    }

    #[test]
    fn reindent_leaves_a_blank_line_empty() {
        assert_eq!(reindented(&["{", "", "x", "}"], 0), ["", "", "    ", ""]);
    }

    #[test]
    fn reindent_clamps_an_over_closed_file_at_column_zero() {
        assert_eq!(reindented(&["}", "}", "x"], 0), ["", "", ""]);
    }

    #[test]
    fn reindent_writes_tabs_when_the_options_do() {
        let lines: Vec<String> = ["{", "x", "}"].iter().map(|s| s.to_string()).collect();
        assert_eq!(reindent(&lines, 0, &tabs()), ["", "\t", ""]);
    }

    #[test]
    fn step_follows_tab_width_until_shiftwidth_says_otherwise() {
        assert_eq!(spaces().step(), 4);
        assert_eq!(Indent { tab_width: 8, ..spaces() }.step(), 8);
        assert_eq!(Indent { shiftwidth: 2, tab_width: 8, ..spaces() }.step(), 2);
        assert_eq!(
            Indent { tab_width: 0, ..spaces() }.step(),
            1,
            "a step of nothing is not a step"
        );
    }

    #[test]
    fn render_writes_what_the_options_ask_for() {
        assert_eq!(spaces().render(6), "      ");
        assert_eq!(tabs().render(8), "\t\t");
        // The step need not divide the tab width, and when it does not the
        // remainder is spaces — there is no most of a tab.
        assert_eq!(Indent { shiftwidth: 2, ..tabs() }.render(6), "\t  ");
        assert_eq!(spaces().render(0), "");
    }

    #[test]
    fn display_col_counts_a_tab_to_the_next_stop() {
        assert_eq!(display_col("\tx", 1, 4), 4);
        assert_eq!(display_col("ab\tx", 3, 4), 4, "a tab after two chars still reaches 4");
        assert_eq!(display_col("abcd\tx", 5, 4), 8);
        assert_eq!(width_of("\t\t", 8), 16);
    }

    #[test]
    fn guides_go_at_every_level_and_never_on_the_text() {
        assert_eq!(guide_columns(8, 4).collect::<Vec<_>>(), [0, 4]);
        assert_eq!(guide_columns(0, 4).count(), 0, "nothing to guide");
        assert_eq!(guide_columns(6, 4).collect::<Vec<_>>(), [0, 4], "a ragged indent still gets 4");
        assert_eq!(guide_columns(3, 4).collect::<Vec<_>>(), [0]);
    }

    #[test]
    fn retab_rewrites_the_indent_and_leaves_the_line_alone() {
        assert_eq!(retab("\tx", &spaces()), Some("    ".to_string()));
        assert_eq!(retab("    x", &tabs()), Some("\t".to_string()));
        assert_eq!(retab("    x", &spaces()), None, "already what it should be");
        assert_eq!(retab("x", &tabs()), None, "nothing to convert");
    }

    #[test]
    fn retab_keeps_the_width_the_line_had() {
        // Six columns of indent stay six columns. The characters change; where
        // the text starts on screen does not.
        let wide = Indent { tab_width: 8, ..spaces() };
        assert_eq!(retab("\tx", &wide), Some(" ".repeat(8)), "a tab was eight columns wide");
        assert_eq!(
            retab("\t  x", &Indent { expandtab: false, tab_width: 4, ..spaces() }),
            None,
            "a tab and two spaces is six columns, which is how it is already written"
        );
    }

    #[test]
    fn retab_does_not_reach_into_the_line() {
        assert_eq!(
            retab("\tmsg = \"a\tb\";\t// note", &spaces()),
            Some("    ".to_string()),
            "the leading tab only — the ones in the string and the alignment are content"
        );
    }

    #[test]
    fn retab_leaves_a_whitespace_only_line_to_the_trimmer() {
        assert_eq!(retab("\t\t", &spaces()), None);
        assert_eq!(retab("", &spaces()), None);
        assert_eq!(retab("   ", &tabs()), None);
    }

    #[test]
    fn leading_stops_at_the_first_real_character() {
        assert_eq!(leading("  \tfoo  "), "  \t");
        assert_eq!(leading("foo"), "");
        assert_eq!(leading("   "), "   ", "a blank line is all indent");
    }

    #[test]
    fn expand_tabs_leaves_a_line_without_any_alone() {
        assert_eq!(expand_tabs("plain", 4), "plain");
        assert_eq!(expand_tabs("\tx", 4), "    x");
        assert_eq!(expand_tabs("ab\tx", 4), "ab  x");
    }
}
