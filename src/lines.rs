//! Whole rows, rearranged: `:uniq`, `:dedup`, `:reverse`, `:align`.
//!
//! Lines in, lines out, and no knowledge of a buffer anywhere in it — the
//! same shape as `sort.rs`, which is the command these sit beside. Each
//! says what it did, or why it did nothing.
//!
//! See `docs/specs/lines.md`.

/// Which rearrangement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineOp {
    /// `:uniq` — runs of identical adjacent lines become one.
    Uniq,
    /// `:dedup` — every line that already appeared is dropped, wherever it
    /// was; the first stays, in place.
    Dedup,
    /// `:reverse` — last line first.
    Reverse,
    /// `:align seq` — spaces before the first `seq` on each line until they
    /// share a column; `:align! seq` pads after it instead, so what follows
    /// lines up.
    Align { seq: String, after: bool },
}

/// Every command name here, for the range whitelist.
pub const NAMES: &[&str] = &["uniq", "dedup", "reverse", "align"];

impl LineOp {
    /// The op a command name asks for. `None` for a name that is not one;
    /// `Some(Err)` for an argument that does not fit. `force` is the `!`.
    pub fn parse(name: &str, arg: &str, force: bool) -> Option<Result<Self, String>> {
        let bare = |op: LineOp| match arg.trim().is_empty() {
            true => Ok(op),
            false => Err(format!("{name} takes no argument")),
        };
        Some(match name {
            "uniq" => bare(LineOp::Uniq),
            "dedup" => bare(LineOp::Dedup),
            "reverse" => bare(LineOp::Reverse),
            "align" => match arg.trim() {
                "" => Err("align on what? `:align =`".into()),
                seq => Ok(LineOp::Align { seq: seq.into(), after: force }),
            },
            _ => return None,
        })
    }

    /// Fewer rows than this and there is nothing to do.
    pub fn min_rows(&self) -> usize {
        match self {
            LineOp::Align { .. } => 1,
            _ => 2,
        }
    }

    /// What to say when the rows are too few.
    pub fn too_few(&self) -> &'static str {
        match self {
            LineOp::Uniq | LineOp::Dedup => "nothing to drop",
            LineOp::Reverse => "nothing to reverse",
            LineOp::Align { .. } => "nothing to align",
        }
    }

    /// What to say when the rows came back as they went in.
    pub fn unchanged(&self) -> String {
        match self {
            LineOp::Uniq | LineOp::Dedup => "nothing to drop".into(),
            LineOp::Reverse => "nothing to reverse".into(),
            LineOp::Align { seq, .. } => format!("already aligned on `{seq}`"),
        }
    }

    /// The rows rearranged, and the report. `tab_width` is for `:align`,
    /// which measures columns on screen rather than in characters.
    pub fn apply(
        &self,
        lines: Vec<String>,
        tab_width: usize,
    ) -> Result<(Vec<String>, String), String> {
        match self {
            LineOp::Uniq => {
                let before = lines.len();
                let mut lines = lines;
                lines.dedup();
                let report = dropped(before - lines.len());
                Ok((lines, report))
            }
            LineOp::Dedup => {
                let before = lines.len();
                let mut seen = std::collections::HashSet::new();
                let lines: Vec<String> =
                    lines.into_iter().filter(|line| seen.insert(line.clone())).collect();
                let report = dropped(before - lines.len());
                Ok((lines, report))
            }
            LineOp::Reverse => {
                let count = lines.len();
                let mut lines = lines;
                lines.reverse();
                Ok((lines, format!("{count} lines reversed")))
            }
            LineOp::Align { seq, after } => align(lines, seq, *after, tab_width),
        }
    }
}

fn dropped(n: usize) -> String {
    format!("{n} line{} dropped", if n == 1 { "" } else { "s" })
}

/// Pads with spaces, and only ever adds: a line that already had its `seq`
/// past the column is the column everyone else moves to.
fn align(
    lines: Vec<String>,
    seq: &str,
    after: bool,
    tab_width: usize,
) -> Result<(Vec<String>, String), String> {
    // Where the padding goes on each line, and the column it is at now.
    let cuts: Vec<Option<(usize, usize)>> = lines
        .iter()
        .map(|line| {
            let at = line.find(seq)?;
            let cut = if after { at + seq.len() } else { at };
            Some((cut, crate::indent::width_of(&line[..cut], tab_width)))
        })
        .collect();
    let Some(target) = cuts.iter().flatten().map(|(_, col)| *col).max() else {
        return Err(format!("no `{seq}` here"));
    };
    let mut touched = 0;
    let lines = lines
        .into_iter()
        .zip(&cuts)
        .map(|(line, cut)| match cut {
            Some((cut, col)) => {
                touched += 1;
                format!("{}{}{}", &line[..*cut], " ".repeat(target - col), &line[*cut..])
            }
            None => line,
        })
        .collect();
    Ok((lines, format!("{touched} line{} aligned", if touched == 1 { "" } else { "s" })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(name: &str, arg: &str, force: bool, lines: &[&str]) -> (Vec<String>, String) {
        LineOp::parse(name, arg, force)
            .unwrap()
            .unwrap()
            .apply(lines.iter().map(|l| l.to_string()).collect(), 4)
            .unwrap()
    }

    #[test]
    fn every_name_parses_and_a_stranger_does_not() {
        for name in NAMES {
            assert!(LineOp::parse(name, "=", false).is_some(), "{name}");
        }
        assert!(LineOp::parse("shuffle", "", false).is_none());
        assert_eq!(
            LineOp::parse("uniq", "x", false).unwrap().unwrap_err(),
            "uniq takes no argument"
        );
        assert_eq!(
            LineOp::parse("align", "", false).unwrap().unwrap_err(),
            "align on what? `:align =`"
        );
    }

    #[test]
    fn uniq_is_adjacent_and_dedup_is_anywhere() {
        let (lines, report) = run("uniq", "", false, &["a", "a", "b", "a"]);
        assert_eq!(lines, ["a", "b", "a"]);
        assert_eq!(report, "1 line dropped");

        let (lines, report) = run("dedup", "", false, &["a", "a", "b", "a"]);
        assert_eq!(lines, ["a", "b"], "the first stays, in place");
        assert_eq!(report, "2 lines dropped");
    }

    #[test]
    fn reverse_reverses() {
        let (lines, report) = run("reverse", "", false, &["1", "2", "3"]);
        assert_eq!(lines, ["3", "2", "1"]);
        assert_eq!(report, "3 lines reversed");
    }

    #[test]
    fn align_pads_before_the_sequence_and_bang_pads_after_it() {
        let (lines, report) = run("align", "=", false, &["a = 1", "bbb = 2", "no", "cc  = 3"]);
        assert_eq!(
            lines,
            ["a   = 1", "bbb = 2", "no", "cc  = 3"],
            "the double space set the column"
        );
        assert_eq!(report, "3 lines aligned");

        let (lines, _) = run("align", ":", true, &["key: v", "longkey: v"]);
        assert_eq!(lines, ["key:     v", "longkey: v"]);
    }

    #[test]
    fn align_measures_tabs_on_screen() {
        let (lines, _) = run("align", "=", false, &["\ta = 1", "bbbbb = 2"]);
        assert_eq!(lines, ["\ta = 1", "bbbbb = 2"], "a tab is four columns here: already level");
    }

    #[test]
    fn align_with_no_match_says_so() {
        let op = LineOp::parse("align", "=", false).unwrap().unwrap();
        assert_eq!(op.apply(vec!["a".into()], 4).unwrap_err(), "no `=` here");
    }
}
