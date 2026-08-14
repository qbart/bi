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
    WordForward,
    WordBackward,
    LineStart,
    LineEnd,
    FirstLine,
    LastLine,
    /// 1-based, as the user typed it.
    Line(usize),
    /// The doubled operator form — the `dd` in `dd`, the `cc` in `cc`. Covers
    /// `count` whole lines starting at the cursor's.
    CurrentLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    Delete,
    Change,
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
            Motion::Left
            | Motion::Right
            | Motion::WordForward
            | Motion::WordBackward
            | Motion::LineStart => Kind::Exclusive,
        }
    }

    /// Whether a count picks a destination rather than repeating the motion.
    /// `5G` goes to line 5; it does not go to the last line five times.
    pub fn is_absolute(self) -> bool {
        matches!(self, Motion::FirstLine | Motion::LastLine | Motion::Line(_))
    }
}
