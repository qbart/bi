//! Drives the editor through the library's public API alone.
//!
//! The assertions matter less than the fact that this file compiles. It is an
//! embedder: it links `bee` and never names a terminal, so it can only build if
//! the core is genuinely frontend-free. If someone adds a `ratatui` type to a
//! signature in `editor`, `input` or `buffer`, this breaks.

use bee::editor::{Editor, Mode};
use bee::input::Input;
use bee::key::{Key, KeyCode};

/// A headless editor session: feed keys, read text back.
struct Session {
    editor: Editor,
    input: Input,
}

impl Session {
    fn new(text: &str) -> Self {
        let mut session = Self { editor: Editor::empty(), input: Input::default() };
        if !text.is_empty() {
            session.type_text(text);
            session.press(Key::code(KeyCode::Esc));
        }
        session
    }

    fn press(&mut self, key: Key) {
        if let Some(cmd) = self.input.on_key(key, &self.editor.mode) {
            self.editor.apply(cmd);
        }
        self.editor.sync_syntax();
    }

    /// Presses each char as its own key, in whatever mode the editor is in.
    fn keys(&mut self, keys: &str) {
        for c in keys.chars() {
            self.press(Key::char(c));
        }
    }

    /// Enters insert mode, types, and stays there.
    fn type_text(&mut self, text: &str) {
        self.press(Key::char('i'));
        for c in text.chars() {
            match c {
                '\n' => self.press(Key::code(KeyCode::Enter)),
                c => self.press(Key::char(c)),
            }
        }
    }

    fn text(&self) -> String {
        self.editor.buffer.rope().to_string()
    }
}

#[test]
fn an_embedder_can_type_move_and_delete_without_a_terminal() {
    let mut s = Session::new("hello world");
    assert_eq!(s.text(), "hello world");

    // Back to the start, then delete the first word.
    s.keys("0dw");
    assert_eq!(s.text(), "world");
}

#[test]
fn counts_and_operators_compose_through_the_public_api() {
    let mut s = Session::new("one two three four five");
    s.keys("0d3w");
    assert_eq!(s.text(), "four five");
}

#[test]
fn undo_and_redo_round_trip() {
    let mut s = Session::new("keep this");
    s.keys("0dw");
    assert_eq!(s.text(), "this");

    s.keys("u");
    assert_eq!(s.text(), "keep this");

    s.press(Key::ctrl('r'));
    assert_eq!(s.text(), "this");
}

#[test]
fn yank_and_paste_travel_through_the_register_ring() {
    let mut s = Session::new("alpha");
    s.keys("0yyp");
    assert_eq!(s.text(), "alpha\nalpha");
}

#[test]
fn modes_are_observable_from_outside() {
    let mut s = Session::new("");
    assert_eq!(s.editor.mode, Mode::Normal);

    s.press(Key::char('i'));
    assert_eq!(s.editor.mode, Mode::Insert);

    s.press(Key::code(KeyCode::Esc));
    assert_eq!(s.editor.mode, Mode::Normal);

    s.press(Key::char(':'));
    assert!(matches!(s.editor.mode, Mode::Command(_)));
}

/// The modules `lib.rs` declares. Kept as a literal list rather than a
/// directory walk so that adding a module is a deliberate decision about which
/// side of the boundary it lands on.
const LIB_MODULES: &[&str] = &[
    "lib.rs",
    "buffer.rs",
    "editor.rs",
    "history.rs",
    "input.rs",
    "key.rs",
    "motion.rs",
    "picker.rs",
    "registers.rs",
    "syntax.rs",
];

/// A lib and a bin in one package share a dependency list, so the compiler will
/// not stop `editor.rs` from importing ratatui. This will.
///
/// Comment lines are skipped: the doc comments explaining the boundary name the
/// very crates they forbid, and a check that fails on its own documentation
/// gets deleted rather than fixed.
#[test]
fn no_library_module_names_a_terminal_crate() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut violations = Vec::new();

    for module in LIB_MODULES {
        let path = root.join(module);
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} is declared in lib.rs but unreadable: {e}", module));

        for (n, line) in source.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if code.contains("ratatui") || code.contains("crossterm") {
                violations.push(format!("{}:{}: {}", module, n + 1, line.trim()));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "terminal crates reached into the library — move this to src/tui/:\n{}",
        violations.join("\n"),
    );
}

/// `lib.rs` and the list above have to agree, or the check above silently stops
/// covering a module.
#[test]
fn the_module_list_matches_what_lib_rs_declares() {
    let lib = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"),
    )
    .unwrap();

    let declared: Vec<String> = lib
        .lines()
        .filter_map(|l| l.trim().strip_prefix("pub mod ")?.strip_suffix(';'))
        .map(|m| format!("{m}.rs"))
        .collect();

    let mut expected: Vec<&str> = LIB_MODULES.iter().copied().filter(|m| *m != "lib.rs").collect();
    let mut declared: Vec<&str> = declared.iter().map(|s| s.as_str()).collect();
    expected.sort_unstable();
    declared.sort_unstable();

    assert_eq!(
        declared, expected,
        "LIB_MODULES is out of step with lib.rs's `pub mod` declarations",
    );
}

#[test]
fn arrow_keys_move_without_any_terminal_types_involved() {
    let mut s = Session::new("ab\ncd");
    s.press(Key::code(KeyCode::Home));
    s.press(Key::code(KeyCode::Up));
    assert_eq!(s.editor.buffer.cursor_row(), 0);
    assert_eq!(s.editor.buffer.cursor_col(), 0);

    s.press(Key::code(KeyCode::Down));
    s.press(Key::code(KeyCode::End));
    assert_eq!(s.editor.buffer.cursor_row(), 1);
}
