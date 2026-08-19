//! Drives the editor through the library's public API alone.
//!
//! The assertions matter less than the fact that this file compiles. It is an
//! embedder: it links `bi` and never names a terminal, so it can only build if
//! the core is genuinely frontend-free. If someone adds a `ratatui` type to a
//! signature in `editor`, `input` or `buffer`, this breaks.

use bi::editor::{Action, Command, Editor, Mode, WindowCmd};
use bi::input::Input;
use bi::key::{Key, KeyCode};
use bi::tree::Kind;
use bi::window::{Chrome, Content, Dir, Rect, Side};

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
        // Whichever keymap the focused window wants — the frontend's whole
        // part in the tree, and it names no terminal to ask.
        let content = self.editor.content_kind();
        if let Some(cmd) = self.input.on_key(key, &self.editor.session.mode, content) {
            self.editor.apply(cmd);
        }
        self.editor.settle();
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
        self.editor.buffer().unwrap().rope().to_string()
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
    assert_eq!(s.editor.session.mode, Mode::Normal);

    s.press(Key::char('i'));
    assert_eq!(s.editor.session.mode, Mode::Insert);

    s.press(Key::code(KeyCode::Esc));
    assert_eq!(s.editor.session.mode, Mode::Normal);

    s.press(Key::char(':'));
    assert!(matches!(s.editor.session.mode, Mode::Command(_)));
}

/// Windows without a terminal.
///
/// Geometry is in the library, so an embedder lays out, splits, switches and
/// edits through the public API alone — and `Rect` is four integers rather than
/// anything a frontend owns.
#[test]
fn an_embedder_can_split_switch_and_edit_in_both_windows() {
    let mut s = Session::new("alpha");
    let chrome = Chrome { columns: 1, rows: 0, min_width: 8, min_height: 2, tree_width: 30 };

    let panes = s.editor.layout(Rect::new(0, 0, 80, 24), chrome);
    assert_eq!(panes.len(), 1);

    s.editor.apply(Command {
        count: 1,
        action: Action::Window(WindowCmd::Split { dir: Dir::Vertical, path: None }),
    });
    let panes = s.editor.layout(Rect::new(0, 0, 80, 24), chrome);
    assert_eq!(panes.len(), 2, "two panes, tiling the area");
    assert_eq!(panes[0].1.width + panes[1].1.width + 1, 80);

    // Type in the new window; the other one is looking at the same text.
    s.keys("0");
    s.type_text("X");
    s.press(Key::code(KeyCode::Esc));
    assert_eq!(s.text(), "Xalpha");

    let other = *s.editor.window_ids().iter().find(|&&id| id != s.editor.focus()).unwrap();
    assert_eq!(s.editor.buffer_of(other).unwrap().rope().to_string(), "Xalpha");

    // The split opened on the right and took focus with it, so the window it
    // came from is the one to the left.
    s.editor.apply(Command { count: 1, action: Action::Window(WindowCmd::Focus(Side::Left)) });
    assert_eq!(s.editor.focus(), other, "switched by geometry");

    s.editor.apply(Command { count: 1, action: Action::Window(WindowCmd::Close) });
    assert_eq!(s.editor.window_ids().len(), 1);
    assert_eq!(s.text(), "Xalpha", "and the buffer outlived the window");
}

/// The modules `lib.rs` declares. Kept as a literal list rather than a
/// directory walk so that adding a module is a deliberate decision about which
/// side of the boundary it lands on.
const LIB_MODULES: &[&str] = &[
    "lib.rs",
    "buffer.rs",
    "clipboard.rs",
    "cmd_history.rs",
    "config/keys.rs",
    "config/mod.rs",
    "config/parse.rs",
    "decoration.rs",
    "editor.rs",
    "editorconfig.rs",
    "history.rs",
    "indent.rs",
    "input.rs",
    "key.rs",
    "motion.rs",
    "picker.rs",
    "registers.rs",
    "selection.rs",
    "syntax.rs",
    "theme.rs",
    "tree.rs",
    "trim.rs",
    "window.rs",
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

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let declared: Vec<String> = lib
        .lines()
        .filter_map(|l| l.trim().strip_prefix("pub mod ")?.strip_suffix(';'))
        .map(|m| {
            // A module can live at `m.rs` or, once it grows submodules, at
            // `m/mod.rs` — check the filesystem rather than guessing.
            if root.join(format!("{m}.rs")).is_file() {
                format!("{m}.rs")
            } else {
                format!("{m}/mod.rs")
            }
        })
        .collect();

    // `pub mod` in `lib.rs` names one entry per module, not one per file — a
    // directory module like `config/` contributes a single `config/mod.rs`
    // here. Submodule files (`config/parse.rs`) are reconciled separately
    // below, against the filesystem rather than against `lib.rs`.
    let mut expected: Vec<&str> = LIB_MODULES
        .iter()
        .copied()
        .filter(|m| *m != "lib.rs" && (!m.contains('/') || m.ends_with("/mod.rs")))
        .collect();
    let mut declared: Vec<&str> = declared.iter().map(|s| s.as_str()).collect();
    expected.sort_unstable();
    declared.sort_unstable();

    assert_eq!(
        declared, expected,
        "LIB_MODULES is out of step with lib.rs's `pub mod` declarations",
    );

    // A directory module can hold more than one file, and `pub mod` in
    // `lib.rs` only names the directory, not what's inside it. Without this,
    // a file added to `config/` (or any future directory module) is invisible
    // to `LIB_MODULES` and so invisible to the terminal-crate scan above —
    // which is exactly how `config/parse.rs` went unchecked.
    for module in LIB_MODULES {
        let Some(dir) = module.strip_suffix("/mod.rs") else { continue };
        let dir_path = root.join(dir);

        let mut on_disk: Vec<String> = std::fs::read_dir(&dir_path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", dir_path.display()))
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                name.ends_with(".rs").then(|| format!("{dir}/{name}"))
            })
            .collect();
        on_disk.sort();

        let mut declared_in_dir: Vec<&str> =
            LIB_MODULES.iter().copied().filter(|m| m.starts_with(&format!("{dir}/"))).collect();
        declared_in_dir.sort_unstable();

        assert_eq!(
            declared_in_dir,
            on_disk.iter().map(String::as_str).collect::<Vec<_>>(),
            "{dir}/ on disk and LIB_MODULES disagree about what files it holds",
        );
    }
}

#[test]
fn arrow_keys_move_without_any_terminal_types_involved() {
    let mut s = Session::new("ab\ncd");
    s.press(Key::code(KeyCode::Home));
    s.press(Key::code(KeyCode::Up));
    assert_eq!(s.editor.cursor_row().unwrap(), 0);
    assert_eq!(s.editor.cursor_col().unwrap(), 0);

    s.press(Key::code(KeyCode::Down));
    s.press(Key::code(KeyCode::End));
    assert_eq!(s.editor.cursor_row().unwrap(), 1);
}

/// A directory under the temp dir, gone when the test ends.
struct ScratchDir(std::path::PathBuf);

impl ScratchDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("bi-embed-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(path.join("src")).unwrap();
        std::fs::write(path.join("src/lib.rs"), "fn main() {}\n").unwrap();
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The tree, driven the way a frontend drives it: keys in, rows out.
///
/// An embedder gets depth and kind per row and draws its own markers. Nothing
/// in here knows what a `▸` is, which is the point.
#[test]
fn an_embedder_can_browse_expand_and_open_without_a_terminal() {
    let dir = ScratchDir::new("browse");
    let mut session =
        Session { editor: Editor::open(dir.path()).unwrap(), input: Input::default() };
    session.editor.layout(
        Rect::new(0, 0, 80, 24),
        Chrome { columns: 1, rows: 0, min_width: 8, min_height: 2, tree_width: 30 },
    );

    let rows = session.editor.window().tree().unwrap().rows();
    assert_eq!(rows[0].depth, 0, "the root");
    assert!(rows.iter().any(|r| r.name == "src" && r.kind == Kind::Dir));

    // Down onto src/, open it, down onto lib.rs, open that.
    session.keys("jl");
    let tree = session.editor.window().tree().unwrap();
    assert!(tree.rows().iter().any(|r| r.name == "lib.rs" && r.depth == 2), "expanded");

    session.keys("j");
    session.press(Key::code(KeyCode::Enter));

    assert_eq!(session.text(), "fn main() {}\n", "the file opened in this window");
    assert!(
        matches!(session.editor.window().alt, Some(Content::Tree(_))),
        "and the tree it displaced is the alternate",
    );
}

/// The three filesystem commands, with no tree involved at all — which is why
/// they are ex commands rather than tree-only actions.
#[test]
fn an_embedder_can_create_rename_and_delete_files() {
    let dir = ScratchDir::new("files");
    let mut editor = Editor::open(dir.path()).unwrap();
    let made = dir.path().join("pkg/notes.md");

    editor.run_ex(&format!("create {}", made.display()));
    assert!(made.is_file(), "with its parent made along the way");

    let moved = dir.path().join("pkg/NOTES.md");
    editor.run_ex(&format!("rename {} {}", made.display(), moved.display()));
    assert!(moved.is_file() && !made.exists());

    editor.run_ex(&format!("delete {}", moved.display()));
    assert!(!moved.exists());
}
