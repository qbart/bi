//! The vocabulary movement and operators share.
//!
//! Motions are data rather than actions so that one description of "where `w`
//! goes" serves both `w` (move there) and `dw` (delete to there). This module
//! holds no logic beyond classification — resolving a motion to a position
//! needs the rope, so that lives on [`crate::buffer::Buffer`].
//!
//! It deliberately depends on nothing, because `buffer`, `editor` and `input`
//! all import it and `buffer` must not end up depending on `editor`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    /// `w` `b` `e` `ge`, and their WORD forms `W` `B` `E` `gE`.
    ///
    /// One variant rather than eight, because the three flags are genuinely
    /// independent and the keymap in `docs/specs/config.md` has to name each
    /// combination anyway. `big` is the whitespace-delimited WORD, where
    /// punctuation does not break a word; `end` lands on the last character
    /// of the word instead of the first, which is also what makes it
    /// inclusive.
    Word {
        big: bool,
        forward: bool,
        end: bool,
    },
    LineStart,
    /// `^` — the first non-blank of the line. Separate from [`Motion::LineStart`]
    /// rather than replacing it, because `0` still has to mean column zero.
    FirstNonBlank,
    /// `g_` — the last non-blank. Inclusive, like `$`, so `dg_` takes the
    /// final character and leaves the trailing whitespace.
    LastNonBlank,
    /// `%` — the bracket matching the one at or after the cursor. Inclusive,
    /// so `d%` takes both brackets and everything between them.
    MatchingBracket,
    /// `{` and `}` — to the blank line that bounds this paragraph.
    Paragraph {
        forward: bool,
    },
    LineEnd,
    FirstLine,
    LastLine,
    /// 1-based, as the user typed it.
    Line(usize),
    /// The doubled operator form — the `dd` in `dd`, the `cc` in `cc`. Covers
    /// `count` whole lines starting at the cursor's.
    CurrentLine,
    /// `f{c}` `F{c}` `t{c}` `T{c}` — within the current line only, which is
    /// what keeps them cheap and is also what vim does.
    FindChar {
        ch: char,
        forward: bool,
        till: bool,
        /// Set when this came from `;` or `,` rather than being typed fresh.
        ///
        /// Only `t`/`T` care. A fresh `t.` from a position already next to the
        /// target stays put, but `;` from there has to skip to the following
        /// match or it would never advance. Vim exposes the same distinction
        /// through `cpo`'s `;` flag.
        repeat: bool,
    },
    /// An absolute char index a search resolved to.
    ///
    /// `Editor` turns [`Motion::Search`] into one of these: it holds the
    /// pattern, so by the time `Buffer` resolves the motion there is nothing
    /// left to match, only somewhere to go.
    Found(usize),
    /// `/` `?` `n` `N` `*` `#`. Carries no pattern for the same reason
    /// [`Motion::RepeatFind`] carries no character — the search lives on
    /// `Editor`, which substitutes the real one before this resolves.
    Search {
        reverse: bool,
    },
    /// `;` and `,`. Carries no character: the last find lives on `Editor`,
    /// because it has to survive the keymap's `reset()` between commands.
    /// `Editor` substitutes the real [`Motion::FindChar`] before resolving.
    RepeatFind {
        reverse: bool,
    },
}

/// A range *containing* the cursor, rather than somewhere to move to.
///
/// This is why `iw` cannot be spelled as a [`Motion`]: a motion answers "where
/// does this go from here", and `iw` answers "what is the word I am inside".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextObject {
    /// `iw` / `iW` — `big` is the whitespace-delimited WORD.
    Word { big: bool },
    /// `i"` `i'` `` i` `` — the quote character itself.
    Quoted(char),
    /// `i(` `i[` `i{` `i<` — identified by the opening bracket.
    Delimited(char),
    /// `ip`
    Paragraph,
}

/// What an operator applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Motion(Motion),
    /// `around` is the `a` of `aw` — the object plus its surroundings.
    Object {
        object: TextObject,
        around: bool,
    },
}

impl Target {
    pub fn kind(self) -> Kind {
        match self {
            Target::Motion(m) => m.kind(),
            // A text object already names an explicit range, so `kind` only has
            // to say how the captured text behaves when pasted back.
            Target::Object { object: TextObject::Paragraph, .. } => Kind::Linewise,
            Target::Object { .. } => Kind::Exclusive,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    Delete,
    Change,
    /// Copies without removing. The only operator that leaves the text alone.
    Yank,
    /// `>` and `<`. An operator in the full sense — it takes a motion, it
    /// doubles, it takes a count, `.` repeats it — which is the only reason
    /// `>j` needs no machinery of its own beside the machinery `dj` already
    /// has.
    ///
    /// The one that captures nothing: there is no register in an indent, so
    /// the `sink` an `Operate` carries alongside it is ignored. It is also
    /// always linewise, whatever its motion says, since half a line cannot be
    /// indented. See `docs/specs/indent.md`.
    Indent {
        right: bool,
    },
    /// `gq` — rewrap to `textwidth`. Captures nothing and is always linewise,
    /// like [`Operator::Indent`], and for the same reasons. See
    /// `docs/specs/reflow.md`.
    Reflow,
    /// `=` — reindent to what the structure wants, which today is bracket
    /// depth. The third indent operator; captures nothing, always linewise.
    /// See `docs/specs/indent.md`.
    Reindent,
}

/// How a motion's endpoints turn into a range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Stops before the char the motion lands on. Most motions.
    Exclusive,
    /// Includes the char the motion lands on — `$` is the one that matters.
    Inclusive,
    /// Whole lines, regardless of column.
    Linewise,
}

impl Motion {
    pub fn kind(self) -> Kind {
        match self {
            Motion::Up | Motion::Down | Motion::FirstLine | Motion::LastLine => Kind::Linewise,
            Motion::Line(_) | Motion::CurrentLine => Kind::Linewise,
            Motion::LineEnd => Kind::Inclusive,
            // Vim's asymmetry, and it is load-bearing: forward finds are
            // inclusive so `df)` takes the `)`, backward ones are exclusive so
            // `dF(` stops before the `(`. Both then do the obvious thing.
            Motion::FindChar { forward, .. } => {
                if forward {
                    Kind::Inclusive
                } else {
                    Kind::Exclusive
                }
            }
            // Exclusive, so `d/three` stops before the match and leaves it.
            Motion::Search { .. } | Motion::Found(_) => Kind::Exclusive,
            // Never actually asked: `Editor::resolve_find` turns this into a
            // `FindChar` — which carries the real direction — before anything
            // resolves it. Exclusive is the conservative answer if it ever
            // leaked, since it deletes less rather than more.
            Motion::RepeatFind { .. } => Kind::Exclusive,
            // `de` takes the word and `dw` takes the word plus the space after
            // it. That difference is the whole reason both keys exist, and it
            // is this line.
            Motion::Word { end, .. } => {
                if end {
                    Kind::Inclusive
                } else {
                    Kind::Exclusive
                }
            }
            Motion::LastNonBlank | Motion::MatchingBracket => Kind::Inclusive,
            Motion::Left
            | Motion::Right
            | Motion::FirstNonBlank
            | Motion::Paragraph { .. }
            | Motion::LineStart => Kind::Exclusive,
        }
    }

    /// Whether a count picks a destination rather than repeating the motion.
    /// `5G` goes to line 5; it does not go to the last line five times.
    pub fn is_absolute(self) -> bool {
        // `Found` names a destination outright, so a count must not repeat it.
        matches!(self, Motion::FirstLine | Motion::LastLine | Motion::Line(_) | Motion::Found(_))
    }
}
