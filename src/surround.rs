//! What a character means as a surrounding.
//!
//! Two questions, and they have different answers for the same key: what to
//! *write* when you ask for `(`, and what to *find* when you ask for it. `(`
//! writes a space inside and `)` does not, while both find the same pair of
//! parentheses.
//!
//! See `docs/specs/surround.md`.

use crate::motion::TextObject;

/// The strings to write on either side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pair {
    pub open: String,
    pub close: String,
}

/// What to write for `ch`, or `None` if it surrounds nothing.
///
/// The open side adds a space inside and the close side does not, which is
/// vim-surround's rule and earns its keep: `ysiw{` gives `{ x }`, which is
/// what a formatter would have written, and `}` gives `{x}` for the times it
/// would not.
pub fn pair_for(ch: char) -> Option<Pair> {
    let (open, close) = match ch {
        '(' => ("( ", " )"),
        ')' | 'b' => ("(", ")"),
        '{' => ("{ ", " }"),
        '}' | 'B' => ("{", "}"),
        '[' => ("[ ", " ]"),
        ']' => ("[", "]"),
        '<' => ("< ", " >"),
        '>' => ("<", ">"),
        '"' => ("\"", "\""),
        '\'' => ("'", "'"),
        '`' => ("`", "`"),
        _ => return None,
    };
    Some(Pair { open: open.to_string(), close: close.to_string() })
}

/// What to *find* for `ch` — the text object whose innards it names.
///
/// Every one of the six bracket characters finds the same pair, so `ds(`,
/// `ds)` and `dsb` all delete the nearest enclosing parentheses. Only writing
/// tells them apart.
pub fn object_for(ch: char) -> Option<TextObject> {
    Some(match ch {
        '(' | ')' | 'b' => TextObject::Delimited('('),
        '{' | '}' | 'B' => TextObject::Delimited('{'),
        '[' | ']' => TextObject::Delimited('['),
        '<' | '>' => TextObject::Delimited('<'),
        '"' | '\'' | '`' => TextObject::Quoted(ch),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_open_side_adds_a_space_inside_and_the_close_side_does_not() {
        assert_eq!(pair_for('(').unwrap(), Pair { open: "( ".into(), close: " )".into() });
        assert_eq!(pair_for(')').unwrap(), Pair { open: "(".into(), close: ")".into() });
        assert_eq!(pair_for('b').unwrap(), pair_for(')').unwrap());
        assert_eq!(pair_for('B').unwrap(), pair_for('}').unwrap());
    }

    #[test]
    fn a_quote_is_the_same_character_on_both_sides() {
        let pair = pair_for('"').unwrap();
        assert_eq!(pair.open, pair.close);
        assert_eq!(pair.open, "\"");
    }

    #[test]
    fn every_bracket_character_finds_the_same_pair() {
        for ch in ['(', ')', 'b'] {
            assert_eq!(object_for(ch), Some(TextObject::Delimited('(')));
        }
        for ch in ['{', '}', 'B'] {
            assert_eq!(object_for(ch), Some(TextObject::Delimited('{')));
        }
    }

    #[test]
    fn a_character_that_surrounds_nothing_says_so() {
        assert!(pair_for('x').is_none());
        assert!(object_for('x').is_none());
        // `t` is tags, which is a parse rather than a pair — see the spec.
        assert!(object_for('t').is_none());
    }
}
