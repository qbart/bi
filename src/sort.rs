//! Which order `:sort` puts lines in.
//!
//! The parsing here and the ordering here, and no knowledge of a buffer
//! anywhere in it — flags in, a [`Sort`] out, and a pure function over
//! strings. The command joins this to a region the way `:s` joins
//! `substitute::parse` to one.
//!
//! See `docs/specs/sort.md`.

use std::cmp::Ordering;

/// How to order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sort {
    /// `:sort!` — descending.
    pub reverse: bool,
    /// `n` — compare the first number on each line.
    pub numeric: bool,
    /// `i` — compare without case.
    pub ignore_case: bool,
    /// `u` — drop lines that compare equal, keeping the first.
    pub unique: bool,
}

/// Reads the flags. `reverse` is the caller's, because the `!` sits on the
/// command name rather than in the argument.
pub fn parse(arg: &str, reverse: bool) -> Result<Sort, String> {
    let mut how = Sort { reverse, ..Sort::default() };
    for c in arg.chars().filter(|c| !c.is_whitespace()) {
        match c {
            'n' => how.numeric = true,
            'i' => how.ignore_case = true,
            'u' => how.unique = true,
            other => return Err(format!("`{other}` is not a sort flag — n, i, u")),
        }
    }
    Ok(how)
}

/// The first integer on the line — an optional `-` and digits, wherever they
/// first appear, so `item 12` sorts by 12. `None` for a line without one,
/// and `None` orders before every number, which is vim.
fn first_number(line: &str) -> Option<i64> {
    let bytes: Vec<char> = line.chars().collect();
    let start = bytes.iter().position(|c| c.is_ascii_digit())?;
    let negative = start > 0 && bytes[start - 1] == '-';
    let digits: String = bytes[start..].iter().take_while(|c| c.is_ascii_digit()).collect();
    let n: i64 = digits.parse().unwrap_or(i64::MAX);
    Some(if negative { n.checked_neg().unwrap_or(i64::MIN) } else { n })
}

fn compare(a: &str, b: &str, how: &Sort) -> Ordering {
    if how.numeric {
        // Numbers and nothing else, so `i` beside `n` has nothing to add.
        return first_number(a).cmp(&first_number(b));
    }
    match how.ignore_case {
        true => a.to_lowercase().cmp(&b.to_lowercase()),
        false => a.cmp(b),
    }
}

/// Orders `lines`, and says how many `u` dropped.
///
/// The sort is stable, so ties keep the order they had. `u` keeps the first
/// of each run of equal lines, under whatever comparison the flags chose.
/// The reverse comes last, `u` included: `:sort! u` is the unique lines,
/// largest first.
pub fn sort_lines(mut lines: Vec<String>, how: &Sort) -> (Vec<String>, usize) {
    lines.sort_by(|a, b| compare(a, b, how));
    let mut dropped = 0;
    if how.unique {
        let before = lines.len();
        lines.dedup_by(|a, b| compare(a, b, how) == Ordering::Equal);
        dropped = before - lines.len();
    }
    if how.reverse {
        lines.reverse();
    }
    (lines, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(lines: &[&str], how: &Sort) -> Vec<String> {
        sort_lines(lines.iter().map(|l| l.to_string()).collect(), how).0
    }

    #[test]
    fn bare_and_every_flag_parse_in_any_order() {
        assert_eq!(parse("", false).unwrap(), Sort::default());
        assert_eq!(parse("n", false).unwrap(), Sort { numeric: true, ..Sort::default() });
        assert_eq!(parse("un", false).unwrap(), parse("nu", false).unwrap());
        assert_eq!(parse("n u i", true).unwrap(), parse("iun", true).unwrap());
        assert!(parse("", true).unwrap().reverse);
    }

    #[test]
    fn an_unknown_flag_is_an_error_naming_it() {
        assert_eq!(parse("x", false).unwrap_err(), "`x` is not a sort flag — n, i, u");
    }

    #[test]
    fn ascending_is_the_default_and_bang_descends() {
        assert_eq!(sorted(&["b", "c", "a"], &Sort::default()), ["a", "b", "c"]);
        let how = Sort { reverse: true, ..Sort::default() };
        assert_eq!(sorted(&["b", "c", "a"], &how), ["c", "b", "a"]);
    }

    #[test]
    fn numeric_reads_the_first_number_wherever_it_sits() {
        let how = Sort { numeric: true, ..Sort::default() };
        assert_eq!(
            sorted(&["item 12", "item 9", "item 100"], &how),
            ["item 9", "item 12", "item 100"]
        );
        assert_eq!(sorted(&["x -2", "x -10", "x 1"], &how), ["x -10", "x -2", "x 1"]);
    }

    #[test]
    fn lines_without_a_number_sort_first_in_the_order_they_had() {
        let how = Sort { numeric: true, ..Sort::default() };
        assert_eq!(sorted(&["b 2", "zeta", "alpha", "a 1"], &how), ["zeta", "alpha", "a 1", "b 2"]);
    }

    #[test]
    fn ignore_case_folds_and_the_sort_is_stable() {
        let how = Sort { ignore_case: true, ..Sort::default() };
        assert_eq!(sorted(&["Beta", "alpha", "BETA"], &how), ["alpha", "Beta", "BETA"]);
    }

    #[test]
    fn unique_keeps_the_first_of_each_run() {
        let how = Sort { unique: true, ..Sort::default() };
        let (lines, dropped) =
            sort_lines(vec!["b".into(), "a".into(), "b".into(), "a".into()], &how);
        assert_eq!(lines, ["a", "b"]);
        assert_eq!(dropped, 2);
    }

    #[test]
    fn unique_compares_the_way_the_other_flags_ask() {
        let how = Sort { unique: true, ignore_case: true, ..Sort::default() };
        assert_eq!(sorted(&["foo", "Foo"], &how), ["foo"], "the first one, case folded away");
    }

    #[test]
    fn reverse_comes_after_unique() {
        let how = Sort { reverse: true, unique: true, ..Sort::default() };
        assert_eq!(sorted(&["a", "c", "a", "b"], &how), ["c", "b", "a"]);
    }
}
