//! `f`/`F`/`t`, counts, and `;`/`,` repeat — through real keypresses, the
//! way fingers do it, so the input chain is covered along with the motion.

use bi::editor::Editor;
use bi::input::Input;
use bi::key::{Key, KeyCode};

struct S {
    editor: Editor,
    input: Input,
}

impl S {
    fn new(text: &str) -> Self {
        let mut s = Self { editor: Editor::empty(), input: Input::default() };
        s.press(Key::char('i'));
        for c in text.chars() {
            match c {
                '\n' => s.press(Key::code(KeyCode::Enter)),
                c => s.press(Key::char(c)),
            }
        }
        s.press(Key::code(KeyCode::Esc));
        s.keys("gg0");
        s
    }

    fn press(&mut self, key: Key) {
        let content = self.editor.content_kind();
        if let Some(cmd) = self.input.on_key(key, &self.editor.session.mode, content) {
            self.editor.apply(cmd);
        }
        self.editor.settle();
    }

    fn keys(&mut self, keys: &str) {
        for c in keys.chars() {
            self.press(Key::char(c));
        }
    }

    fn text(&self) -> String {
        self.editor.buffer().unwrap().rope().to_string()
    }

    /// Runs `motion` then marks where the cursor landed with an `X`.
    fn mark(text: &str, motion: &str) -> String {
        let mut s = S::new(text);
        s.keys(motion);
        s.keys("rX");
        s.text()
    }
}

#[test]
fn f_jumps_to_the_next_occurrence() {
    assert_eq!(S::mark("one apple and another apple", "fa"), "one Xpple and another apple");
}

#[test]
fn semicolon_repeats_the_jump_to_the_next_occurrence() {
    assert_eq!(S::mark("one apple and another apple", "fa;"), "one apple Xnd another apple");
    assert_eq!(S::mark("one apple and another apple", "fa;;"), "one apple and Xnother apple");
}

#[test]
fn comma_reverses_the_repeat() {
    assert_eq!(S::mark("one apple and another apple", "fa;;,"), "one apple Xnd another apple");
}

#[test]
fn counted_2fa_reaches_the_second_occurrence_directly() {
    assert_eq!(S::mark("one apple and another apple", "2fa"), "one apple Xnd another apple");
}

#[test]
fn counted_repeat_2_semicolon_skips_ahead() {
    assert_eq!(S::mark("one apple and another apple", "fa2;"), "one apple and Xnother apple");
}

#[test]
fn capital_f_searches_backwards_and_semicolon_keeps_the_direction() {
    assert_eq!(S::mark("one apple and another apple", "$Fp"), "one apple and another apXle");
    assert_eq!(S::mark("one apple and another apple", "$Fp;"), "one apple and another aXple");
    assert_eq!(S::mark("one apple and another apple", "$Fp;,"), "one apple and another apXle");
}

#[test]
fn counted_2_capital_f_reaches_the_second_back() {
    assert_eq!(S::mark("one apple and another apple", "$2Fp"), "one apple and another aXple");
}

#[test]
fn a_missed_find_moves_nothing() {
    assert_eq!(S::mark("one apple", "fz"), "Xne apple", "the cursor stayed at column 0");
}

/// A counted find is one atomic question, as in vim: `3fp` on a line with two
/// `p`s goes nowhere — it must not stop on the second as three separate `fp`s
/// would.
#[test]
fn an_overshooting_count_fails_the_whole_motion() {
    assert_eq!(S::mark("one apple", "3fp"), "Xne apple");
    assert_eq!(S::mark("one apple and another apple", "fa9;"), "one Xpple and another apple");
}

/// `t` with a count has to consume it in one pass: a second fresh `t` from
/// just before a match refuses to advance past it, so `2ta` done as two `ta`s
/// would equal `ta`.
#[test]
fn counted_till_reaches_the_nth_not_the_first() {
    assert_eq!(S::mark("one apple and another apple", "2ta"), "one appleXand another apple");
}

#[test]
fn find_works_as_an_operator_target() {
    let mut s = S::new("one apple and another apple");
    s.keys("dfa");
    assert_eq!(s.text(), "pple and another apple");

    let mut s = S::new("one apple and another apple");
    s.keys("d2fa");
    assert_eq!(s.text(), "nd another apple");
}

#[test]
fn d_semicolon_repeats_the_find_under_the_operator() {
    let mut s = S::new("one apple and another apple");
    s.keys("fa");
    s.keys("d;");
    assert_eq!(s.text(), "one nd another apple");
}
