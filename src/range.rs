//! Which lines a `:` command applies to.
//!
//! `:m 12` names one, `:2,5d` names four, `:%s/…` names all of them. Vim
//! answers that with a small language written in front of the command name,
//! and this is that language: the parsing here, the resolving here, and no
//! knowledge of a buffer anywhere in it.
//!
//! See `docs/specs/ranges.md`.

/// Where an address counts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Base {
    /// `.` — the line the cursor is on, and what a bare offset is measured
    /// from.
    Current,
    /// `$`
    Last,
    /// `12`, counting from one. Zero is a legal thing to write and an illegal
    /// line to be at; whether it means anything is the command's to say.
    Row(usize),
    /// `'<` and `'>` — the first and last line of the primary selection.
    SelectionFirst,
    SelectionLast,
}

/// One line named on a `:` line, before it is resolved against a file.
///
/// A base and a sum of offsets, kept apart because `$-1` is a thing you can
/// name without knowing how long the file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Address {
    pub base: Base,
    pub offset: isize,
}

/// The three line numbers resolution needs, one-based.
///
/// Passed in rather than reached for, which is what keeps this module from
/// ever learning what a `Buffer` is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Where {
    /// How many lines the file has.
    pub lines: usize,
    /// The line the cursor is on.
    pub cursor: usize,
    /// The primary selection's first and last line. Both are the cursor's line
    /// when nothing is selected, which is what makes `'<` harmless there.
    pub selection: (usize, usize),
}

/// A span of lines a `:` command applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineRange {
    pub first: Address,
    pub last: Address,
}

/// What a `:` command acts on, as written on the line.
///
/// Two spellings, because there are two questions and they have different
/// answers over a rectangle or a charwise selection:
///
/// - `'v` is **the selection itself**, whatever shape it has — the columns of
///   a block, the characters of a charwise selection, the rows of a linewise
///   one, and every selection when there are several.
/// - `'<,'>`, like every other address, names **rows**.
///
/// The `:` line prefills `'v` when a selection is up, so the scope is in text
/// you can see and edit before you press Enter. That is the whole of why it
/// cannot be lost on the way to the command: it is not a hidden flag that one
/// accessor reads under one mode, it is the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// `'v`
    Selection,
    /// `%`, `2,5`, `'<,'>`, `$-1` — rows.
    Lines(LineRange),
}

impl Address {
    pub fn at(base: Base) -> Self {
        Self { base, offset: 0 }
    }

    /// The one-based line this names — unclamped and unchecked.
    ///
    /// `0` and past the end are both possible answers, and refusing them is
    /// the caller's job: a range's lines have to exist, and `:m`'s argument is
    /// a line to land *after*, where `0` is the top of the file. One rule
    /// could not serve both, so this one serves neither and says the number.
    pub fn resolve(&self, at: Where) -> isize {
        let base = match self.base {
            Base::Current => at.cursor as isize,
            Base::Last => at.lines as isize,
            Base::Row(n) => n as isize,
            Base::SelectionFirst => at.selection.0 as isize,
            Base::SelectionLast => at.selection.1 as isize,
        };
        base + self.offset
    }

    /// The reading `:m` gives an address over a block of lines: a relative
    /// offset measures from the block's own edge in its direction of travel —
    /// `-2` from the first line, `+1` from the last — never from the cursor,
    /// which may sit at either end of a selection and, inside a tall one,
    /// resolves to a line the block itself occupies. Vim keeps the cursor
    /// reading and errors there (E134), which is why every vimrc spells these
    /// `'<-2` and `'>+1`; bi makes the plain spelling mean what those do.
    /// Every absolute form resolves as [`Address::resolve`].
    pub fn resolve_for_block(&self, at: Where, first: usize, last: usize) -> isize {
        match self.base {
            Base::Current if self.offset != 0 => {
                let edge = if self.offset < 0 { first } else { last };
                edge as isize + self.offset
            }
            _ => self.resolve(at),
        }
    }
}

impl LineRange {
    /// The whole file: `1,$`, which is what `%` is short for.
    pub fn whole() -> Self {
        Self { first: Address::at(Base::Row(1)), last: Address::at(Base::Last) }
    }

    /// The zero-based rows this covers, inclusive, or the message to print.
    ///
    /// Backwards comes back swapped rather than refused: `:5,2` and `:2,5` are
    /// the same four lines, nobody types the first on purpose, and vim's
    /// prompt is a worse interruption than the thing it guards against.
    pub fn rows(&self, at: Where) -> Result<(usize, usize), String> {
        let (a, b) = (self.first.resolve(at), self.last.resolve(at));
        let (lo, hi) = (a.min(b), a.max(b));
        // A typed line number is a claim about a line that either exists or
        // does not, and doing your best with it is how you delete the wrong
        // four lines.
        for n in [lo, hi] {
            if n < 1 || n > at.lines as isize {
                return Err(format!("no line {n}"));
            }
        }
        Ok((lo as usize - 1, hi as usize - 1))
    }
}

/// Reads a range off the front of a `:` line, and hands back what is left.
///
/// Never an error: a line that starts with no address has no range, and comes
/// back whole so the command table reads it exactly as it always did. Nothing
/// bi calls a command starts with `%`, `.`, `$`, `'`, a digit or a sign, which
/// is what makes that safe.
pub fn parse(line: &str) -> (Option<Scope>, &str) {
    let line = line.trim_start();
    // `'v` is the whole scope rather than an address, so it takes no offset
    // and no comma: a rectangle has no first and last line to count from.
    if let Some(rest) = line.strip_prefix("'v") {
        return (Some(Scope::Selection), rest.trim_start());
    }
    if let Some(rest) = line.strip_prefix('%') {
        return (Some(Scope::Lines(LineRange::whole())), rest.trim_start());
    }

    let (first, rest) = match address(line) {
        Some((address, rest)) => (Some(address), rest),
        None => (None, line),
    };
    let Some(after_comma) = rest.trim_start().strip_prefix(',') else {
        // One address is the one line, and no address at all is no range.
        return match first {
            Some(one) => {
                (Some(Scope::Lines(LineRange { first: one, last: one })), rest.trim_start())
            }
            None => (None, line),
        };
    };

    // An omitted address on either side of the comma is `.`, so `:,5` and
    // `:2,` are both a range from or to where you are.
    let here = Address::at(Base::Current);
    let (last, rest) = match address(after_comma.trim_start()) {
        Some((address, rest)) => (address, rest),
        None => (here, after_comma),
    };
    (Some(Scope::Lines(LineRange { first: first.unwrap_or(here), last })), rest.trim_start())
}

/// One address off the front of `s`, and what follows it.
fn address(s: &str) -> Option<(Address, &str)> {
    let digits = |s: &str| s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (base, mut rest) = if let Some(rest) = s.strip_prefix('.') {
        (Base::Current, rest)
    } else if let Some(rest) = s.strip_prefix('$') {
        (Base::Last, rest)
    } else if let Some(rest) = s.strip_prefix("'<") {
        (Base::SelectionFirst, rest)
    } else if let Some(rest) = s.strip_prefix("'>") {
        (Base::SelectionLast, rest)
    } else if s.starts_with(|c: char| c.is_ascii_digit()) {
        let end = digits(s);
        (Base::Row(s[..end].parse().ok()?), &s[end..])
    } else if s.starts_with(['+', '-']) {
        // A bare offset is measured from `.`, which is left to the loop below.
        (Base::Current, s)
    } else {
        return None;
    };

    // Offsets stack — `.+1+1` is `.+2` — because summing them is a loop and
    // refusing them would be a rule to write down.
    let mut offset = 0;
    while let Some(sign) = rest.chars().next().filter(|c| *c == '+' || *c == '-') {
        let after = &rest[1..];
        let end = digits(after);
        // A bare `+` or `-` is one, which is what a finger reaching for the
        // key rather than the number means. Vim reads them the same way.
        let n: isize = match end {
            0 => 1,
            _ => after[..end].parse().ok()?,
        };
        offset += if sign == '+' { n } else { -n };
        rest = &after[end..];
    }
    Some((Address { base, offset }, rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(lines: usize) -> Where {
        Where { lines, cursor: 3, selection: (2, 4) }
    }

    fn one(line: &str) -> (LineRange, &str) {
        match parse(line) {
            (Some(Scope::Lines(range)), rest) => (range, rest),
            _ => panic!("{line:?} has no line range"),
        }
    }

    #[test]
    fn every_base_is_the_line_it_names() {
        let here = at(10);
        let base = |b| Address::at(b).resolve(here);
        assert_eq!(base(Base::Current), 3);
        assert_eq!(base(Base::Last), 10);
        assert_eq!(base(Base::Row(7)), 7);
        assert_eq!(base(Base::SelectionFirst), 2);
        assert_eq!(base(Base::SelectionLast), 4);
    }

    /// The point of keeping the base and the offset apart: "one before the
    /// end" is a thing you can name without knowing how long the file is.
    #[test]
    fn an_offset_rides_on_whatever_it_was_written_after() {
        let last_but_one = one("$-1").0.first;
        assert_eq!(last_but_one.resolve(at(10)), 9);
        assert_eq!(last_but_one.resolve(at(400)), 399);
    }

    #[test]
    fn a_bare_offset_is_measured_from_the_cursor() {
        assert_eq!(one("+3").0.first, Address { base: Base::Current, offset: 3 });
        assert_eq!(one("-2").0.first, Address { base: Base::Current, offset: -2 });
        assert_eq!(one("+").0.first.resolve(at(10)), 4, "a bare sign is one");
        assert_eq!(one("-").0.first.resolve(at(10)), 2);
    }

    #[test]
    fn offsets_stack() {
        assert_eq!(one(".+1+1").0.first.resolve(at(10)), 5);
        assert_eq!(one(".+3-1").0.first.resolve(at(10)), 5);
    }

    #[test]
    fn percent_is_the_whole_file() {
        let (range, rest) = one("%s/a/b/");
        assert_eq!(range, LineRange::whole());
        assert_eq!(range.rows(at(10)), Ok((0, 9)));
        assert_eq!(rest, "s/a/b/", "and the command is untouched");
    }

    #[test]
    fn an_omitted_address_is_the_cursors_line() {
        assert_eq!(one(",5").0.rows(at(10)), Ok((2, 4)), "from `.` to line 5");
        assert_eq!(one("2,").0.rows(at(10)), Ok((1, 2)), "from line 2 to `.`");
    }

    #[test]
    fn one_address_is_one_line() {
        let (range, rest) = one("12d");
        assert_eq!(range.first, range.last);
        assert_eq!(range.rows(at(20)), Ok((11, 11)));
        assert_eq!(rest, "d");
    }

    /// The property every existing command depends on: a line that names no
    /// lines comes back whole.
    #[test]
    fn a_line_that_starts_with_no_address_is_returned_untouched() {
        for line in ["w file.txt", "m+1", "case snake", "e", "noh"] {
            assert_eq!(parse(line), (None, line), "{line}");
        }
    }

    /// `'v` is the selection itself, whatever shape it has — the one thing
    /// `'<,'>` cannot say, because a rectangle has no first and last line.
    #[test]
    fn the_selection_can_be_named_as_itself() {
        assert_eq!(parse("'v case lower"), (Some(Scope::Selection), "case lower"));
        assert_eq!(parse("'vcase lower"), (Some(Scope::Selection), "case lower"));
        assert_eq!(parse("'v"), (Some(Scope::Selection), ""));
    }

    /// It takes no offset and no comma: there is nothing to count from.
    #[test]
    fn the_selection_scope_is_the_whole_of_what_it_says() {
        assert_eq!(parse("'v+1s/a/b/"), (Some(Scope::Selection), "+1s/a/b/"));
    }

    #[test]
    fn a_backwards_range_comes_back_swapped() {
        assert_eq!(one("5,2").0.rows(at(10)), Ok((1, 4)));
    }

    #[test]
    fn a_line_that_is_not_there_is_refused_and_named() {
        assert_eq!(one("2,99").0.rows(at(10)), Err("no line 99".into()));
        assert_eq!(one("0,5").0.rows(at(10)), Err("no line 0".into()));
        assert_eq!(one("$-20").0.rows(at(10)), Err("no line -10".into()));
    }

    /// `'<,'>` is the block you had, which is not the same question as the
    /// block you have — and is why the two spellings exist at all.
    #[test]
    fn the_selection_marks_name_its_two_ends() {
        assert_eq!(one("'<,'>").0.rows(at(10)), Ok((1, 3)));
        assert_eq!(one("'<+1,'>").0.rows(at(10)), Ok((2, 3)));
    }

    #[test]
    fn whitespace_around_the_range_is_not_part_of_it() {
        let (range, rest) = one("  2 , 5  d x");
        assert_eq!(range.rows(at(10)), Ok((1, 4)));
        assert_eq!(rest, "d x");
    }
}
