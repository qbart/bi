//! Reading `:s/old/new/flags`.
//!
//! A string in and a [`Substitute`] out, with no buffer and no editor: every
//! rule in `docs/specs/substitute.md` except "back to front" lives here, which
//! is what lets them be tested without a rope.

/// What a parsed `:s` asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Substitute {
    /// Empty means "the last thing you searched for", which the caller has and
    /// this does not.
    pub pattern: String,
    pub replacement: String,
    /// `g` — every match on a line rather than the first.
    pub all: bool,
    /// `i` / `I`. `None` is smartcase, the same rule `/` follows.
    pub case: Option<bool>,
    /// `n` — say how many there are and change nothing.
    pub count_only: bool,
}

/// Whether `c` can separate the fields of a `:s`.
///
/// Vim's rule: anything but a letter, a digit, whitespace and the three
/// characters that already mean something on a `:` line. Refusing letters is
/// what keeps `:set`, `:sp` and `:split` commands in their own right rather
/// than substitutions with a delimiter of `e` or `p`.
pub fn is_delimiter(c: char) -> bool {
    !c.is_alphanumeric() && !c.is_whitespace() && !matches!(c, '\\' | '"' | '|')
}

/// Reads everything after the command name.
///
/// `arg` starts at the delimiter: for `:%s/a/b/g` it is `/a/b/g`.
pub fn parse(arg: &str) -> Result<Substitute, String> {
    let mut chars = arg.chars();
    let Some(delim) = chars.next().filter(|c| is_delimiter(*c)) else {
        return Err("substitute what? `:s/old/new/`".into());
    };

    // The pattern, then the replacement. A missing closing delimiter is fine —
    // `:%s/old/new` is what everybody types — but a missing separator means
    // there is no replacement to have been left off.
    let (pattern, rest) = take_field(chars.as_str(), delim);
    let Some(rest) = rest else {
        return Err(format!("no `{delim}` after the pattern: `:s{delim}old{delim}new{delim}`"));
    };
    let (replacement, rest) = take_field(rest, delim);

    let mut out = Substitute { pattern, replacement, all: false, case: None, count_only: false };
    for flag in rest.unwrap_or("").chars() {
        match flag {
            'g' => out.all = true,
            'i' => out.case = Some(false),
            'I' => out.case = Some(true),
            'n' => out.count_only = true,
            // Named rather than ignored: a flag that does nothing quietly is
            // how you believe a substitution was case-insensitive.
            c if c.is_whitespace() => {}
            c => return Err(format!("no such flag: `{c}` — one of g, i, I, n")),
        }
    }
    Ok(out)
}

/// One field off the front, up to an unescaped `delim`.
///
/// Returns what was read and what is left *after* the delimiter, or `None`
/// when the field ran to the end of the line without one.
///
/// `\<delim>` is that character and `\\` is a backslash; every other `\` keeps
/// its backslash, because the pattern is literal and `\d` is two characters
/// until there is a regex to make it one.
fn take_field(text: &str, delim: char) -> (String, Option<&str>) {
    let mut out = String::new();
    let mut chars = text.char_indices();
    while let Some((i, c)) = chars.next() {
        if c == delim {
            return (out, Some(&text[i + c.len_utf8()..]));
        }
        if c == '\\' {
            match chars.next() {
                Some((_, next)) if next == delim || next == '\\' => out.push(next),
                Some((_, next)) => {
                    out.push('\\');
                    out.push(next);
                }
                None => out.push('\\'),
            }
            continue;
        }
        out.push(c);
    }
    (out, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(arg: &str) -> Substitute {
        parse(arg).expect("parses")
    }

    #[test]
    fn any_delimiter_reads_the_same_command() {
        let slash = ok("/old/new/");
        assert_eq!(slash.pattern, "old");
        assert_eq!(slash.replacement, "new");
        assert_eq!(ok("#old#new#"), slash);
        assert_eq!(ok(",old,new,"), slash);
    }

    /// `:s#/usr#/opt#` is the whole reason the delimiter is a choice.
    #[test]
    fn a_path_needs_no_escaping_under_another_delimiter() {
        let s = ok("#/usr/local#/opt#");
        assert_eq!((s.pattern.as_str(), s.replacement.as_str()), ("/usr/local", "/opt"));
    }

    #[test]
    fn the_closing_delimiter_is_optional_but_the_separator_is_not() {
        assert_eq!(ok("/old/new"), ok("/old/new/"));
        assert!(parse("/old").is_err(), "no replacement to have been left off");
        assert!(parse("").is_err());
        assert!(parse("x/old/new/").is_err(), "a letter is never a delimiter");
    }

    #[test]
    fn a_backslash_escapes_the_delimiter_and_itself_and_nothing_else() {
        let s = ok(r"/a\/b/c\\d/");
        assert_eq!(s.pattern, "a/b");
        assert_eq!(s.replacement, r"c\d");

        let literal = ok(r"/\d/x/");
        assert_eq!(literal.pattern, r"\d", "two characters until there is a regex");
    }

    #[test]
    fn flags_read_in_any_order_and_an_unknown_one_says_so() {
        let s = ok("/a/b/gi");
        assert!(s.all && s.case == Some(false));
        assert!(ok("/a/b/Ig").all);
        assert_eq!(ok("/a/b/I").case, Some(true));
        assert!(ok("/a/b/n").count_only);

        let err = parse("/a/b/z").unwrap_err();
        assert!(err.contains('z'), "{err}");
    }

    /// `:%s//new/g` — the pattern is the last thing you searched for, which is
    /// the caller's business and not this one's.
    #[test]
    fn an_empty_pattern_parses() {
        assert_eq!(ok("//new/g").pattern, "");
    }

    #[test]
    fn a_delimiter_is_anything_a_command_name_cannot_be() {
        assert!(is_delimiter('/') && is_delimiter('#') && is_delimiter(','));
        assert!(!is_delimiter('e'), "or `:set` would be a substitution");
        assert!(!is_delimiter('2') && !is_delimiter(' '));
        assert!(!is_delimiter('\\') && !is_delimiter('"') && !is_delimiter('|'));
    }
}
