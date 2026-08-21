//! What `:resize` means, as a value.
//!
//! Parsing only: it takes a string and answers what was asked for, with no
//! layout in reach. [`crate::window::Layout`] does the moving, and the editor
//! joins the two — the same split `substitute.rs` and `case.rs` make, and for
//! the same reason: a grammar with its own tests is a grammar you can be sure
//! of before anything on screen has moved.
//!
//! See `docs/specs/resize.md`.

/// How far, and in which of the three ways you can say it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Amount {
    /// `30` — make it this many cells.
    Cells(u16),
    /// `+3`, `-3` — this many cells more or less than now.
    By(i32),
    /// `1:2` — these shares, among the children of the split being divided.
    ///
    /// Held as written rather than normalised, so the error message about the
    /// wrong number of terms can count what you typed — and as whole numbers,
    /// because that is what a ratio of panes is. A pane is not divisible, so
    /// `1.5:2` is refused rather than read as the `3:4` it means.
    Ratio(Vec<u32>),
}

/// One `:resize` line: an amount along either axis, or both.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Resize {
    /// Across — the width of the pane.
    pub x: Option<Amount>,
    /// Down — its height.
    pub y: Option<Amount>,
}

/// Reads a `:resize` argument.
///
/// ```text
/// 30        width, in cells          1:2      widths in these shares
/// 30y       height                   1:2y     heights
/// +3  -3    width, relative          1:2:1    three panes, three shares
/// +3,-3     width then height        1:2,1:2  both axes
/// ```
///
/// **A `y` suffix names the axis; a comma decides it by position.** One part
/// is across unless it says otherwise; two parts are across and then down,
/// which is the order the examples in every editor's help are written in and
/// the order the two numbers are read in everywhere else.
pub fn parse(arg: &str) -> Result<Resize, String> {
    let arg = arg.trim();
    if arg.is_empty() {
        return Err("resize to what? `:resize 30`, `:resize +3`, `:resize 1:2`".into());
    }

    let parts: Vec<&str> = arg.split(',').map(str::trim).collect();
    if parts.len() > 2 {
        return Err("resize takes one amount, or one per axis: `:resize +3,-3`".into());
    }

    // Two passes, because the suffix wins over the position and cannot be
    // known to have won until every part has been read: `:resize 30y,10` is a
    // height and *then* a width, and placing the parts in the order they were
    // written would put both of them on `y` and call it a mistake.
    let mut said: Vec<(Amount, bool)> = Vec::new();
    for part in &parts {
        match part.strip_suffix(['y', 'Y']) {
            Some(body) => said.push((amount(body.trim_end())?, true)),
            None => said.push((amount(part)?, false)),
        }
    }

    let mut out = Resize::default();
    for (amount, _) in said.iter().filter(|(_, explicit)| *explicit) {
        if out.y.is_some() {
            return Err("resize was given the same axis twice".into());
        }
        out.y = Some(amount.clone());
    }
    // Whatever did not name an axis takes the first one still free, across
    // before down.
    for (amount, _) in said.iter().filter(|(_, explicit)| !*explicit) {
        let slot = match (&out.x, &out.y) {
            (None, _) => &mut out.x,
            (_, None) => &mut out.y,
            _ => return Err("resize was given the same axis twice".into()),
        };
        *slot = Some(amount.clone());
    }
    Ok(out)
}

fn amount(body: &str) -> Result<Amount, String> {
    if body.is_empty() {
        return Err("resize by how much?".into());
    }

    if body.contains(':') {
        let mut shares: Vec<u32> = Vec::new();
        for term in body.split(':') {
            let term = term.trim();
            // Whole numbers only. `1:2` is a ratio of panes, and a pane is not
            // divisible — `1.5:2` is `3:4` and someone typing the first meant
            // the second.
            let n: u32 = term.parse().map_err(|_| format!("`{term}` is not a share"))?;
            if n == 0 {
                return Err("a share of 0 is a pane of nothing".into());
            }
            shares.push(n);
        }
        if shares.len() < 2 {
            return Err("a ratio needs at least two shares: `:resize 1:2`".into());
        }
        return Ok(Amount::Ratio(shares));
    }

    if let Some(rest) = body.strip_prefix(['+', '-']) {
        // A bare `+` or `-` is one, which is what a finger reaching for the
        // key rather than the number means — the same reading `ranges.rs`
        // gives an offset.
        let n: i32 = match rest.trim() {
            "" => 1,
            digits => digits.parse().map_err(|_| format!("`{body}` is not a number of cells"))?,
        };
        let sign = if body.starts_with('-') { -1 } else { 1 };
        return Ok(Amount::By(sign * n));
    }

    body.parse::<u16>().map(Amount::Cells).map_err(|_| format!("`{body}` is not a number of cells"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(arg: &str) -> Resize {
        parse(arg).expect(arg)
    }

    #[test]
    fn a_bare_number_is_a_width_in_cells() {
        assert_eq!(ok("30"), Resize { x: Some(Amount::Cells(30)), y: None });
        assert_eq!(ok(" 30 "), Resize { x: Some(Amount::Cells(30)), y: None });
    }

    #[test]
    fn a_y_suffix_makes_it_a_height() {
        assert_eq!(ok("30y"), Resize { x: None, y: Some(Amount::Cells(30)) });
        assert_eq!(ok("+3y"), Resize { x: None, y: Some(Amount::By(3)) });
        assert_eq!(ok("1:2y").y, Some(Amount::Ratio(vec![1, 2])));
    }

    #[test]
    fn a_sign_is_relative_and_a_bare_sign_is_one() {
        assert_eq!(ok("+3").x, Some(Amount::By(3)));
        assert_eq!(ok("-3").x, Some(Amount::By(-3)));
        assert_eq!(ok("+").x, Some(Amount::By(1)), "a finger reaching for the key");
        assert_eq!(ok("-").x, Some(Amount::By(-1)));
    }

    #[test]
    fn a_comma_is_across_and_then_down() {
        assert_eq!(ok("+3,-3"), Resize { x: Some(Amount::By(3)), y: Some(Amount::By(-3)) });
        assert_eq!(
            ok("1:2,1:2"),
            Resize { x: Some(Amount::Ratio(vec![1, 2])), y: Some(Amount::Ratio(vec![1, 2])) }
        );
        assert_eq!(ok("30,10"), Resize { x: Some(Amount::Cells(30)), y: Some(Amount::Cells(10)) });
    }

    #[test]
    fn the_suffix_wins_over_the_position() {
        // Nobody types this, and a letter that is read and then ignored would
        // be worse than a letter that is refused.
        assert_eq!(ok("30y,10"), Resize { x: Some(Amount::Cells(10)), y: Some(Amount::Cells(30)) });
        assert!(parse("30y,10y").is_err(), "the same axis twice says nothing");
    }

    #[test]
    fn a_ratio_is_whole_shares_and_at_least_two() {
        assert_eq!(ok("1:2:1").x, Some(Amount::Ratio(vec![1, 2, 1])));
        assert!(parse("1").is_ok(), "one number is a width, not a ratio");
        assert!(parse("1:").is_err());
        assert!(parse("0:1").is_err(), "a share of 0 is a pane of nothing");
        assert!(parse("1.5:2").is_err(), "a pane is not divisible; that ratio is 3:4");
    }

    #[test]
    fn what_is_not_a_resize_says_so() {
        assert!(parse("").is_err());
        assert!(parse("wide").is_err());
        assert!(parse("+x").is_err());
        assert!(parse("1,2,3").is_err());
        assert!(parse("y").is_err(), "an axis and no amount");
    }
}
