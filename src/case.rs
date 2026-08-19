//! Converting between the ways a name can be spelled.
//!
//! `helloWorld`, `hello_world`, `HelloWorld`, `HELLO_WORLD` and `hello-world`
//! are one name in five spellings, and moving between them by hand is the kind
//! of edit that takes four commands and gets one character wrong.
//!
//! See `docs/specs/case.md`.

/// A spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// `HELLO WORLD` — a pure case mapping, everything else left alone.
    Upper,
    /// `hello world`
    Lower,
    /// `Hello World`
    Title,
    /// `helloWorld`
    Camel,
    /// `HelloWorld`
    Pascal,
    /// `hello_world`
    Snake,
    /// `hello-world`
    Kebab,
    /// `HELLO_WORLD`
    Constant,
}

impl Style {
    /// Every name a style answers to, for `:case` to accept and to complain
    /// with.
    pub const NAMES: &'static [&'static str] =
        &["upper", "lower", "title", "camel", "pascal", "snake", "kebab", "constant"];

    pub fn parse(name: &str) -> Option<Style> {
        Some(match name {
            "upper" => Style::Upper,
            "lower" => Style::Lower,
            // `capital` because that is what people call it when they are not
            // thinking about typography.
            "title" | "capital" => Style::Title,
            "camel" => Style::Camel,
            "pascal" => Style::Pascal,
            "snake" => Style::Snake,
            "kebab" | "dash" => Style::Kebab,
            // `screaming` is the name the internet gave it.
            "constant" | "screaming" => Style::Constant,
            _ => return None,
        })
    }

    /// Whether this style rewrites whole identifiers rather than mapping each
    /// character where it stands.
    fn joins(self) -> bool {
        !matches!(self, Style::Upper | Style::Lower | Style::Title)
    }
}

/// Converts every identifier in `text`, leaving everything between them alone.
///
/// Per identifier rather than over the text as a whole: `foo_bar baz_qux` in
/// camel is `fooBar bazQux`, not `fooBarBazQux`. The second reading is what
/// "convert this to camel" literally says and is never what anybody means.
pub fn convert(text: &str, style: Style) -> String {
    match style {
        Style::Upper => return text.to_uppercase(),
        Style::Lower => return text.to_lowercase(),
        _ => {}
    }

    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let Some(end) = identifier_end(&chars, i) else {
            out.push(chars[i]);
            i += 1;
            continue;
        };
        let word: String = chars[i..end].iter().collect();
        out.push_str(&match style.joins() {
            true => join(&words(&word), style),
            false => title(&word),
        });
        i = end;
    }
    out
}

/// Where the identifier starting at `at` ends, if one starts there.
///
/// A run of letters, digits and underscores, and an inner `-` when letters
/// stand on both sides of it — so `hello-world` is one name while `a - b` is
/// two and a minus sign.
fn identifier_end(chars: &[char], at: usize) -> Option<usize> {
    let part = |c: char| c.is_alphanumeric() || c == '_';
    if !part(chars[at]) {
        return None;
    }
    let mut end = at;
    while end < chars.len() {
        // A `-` only counts *between* two identifier characters, so
        // `hello-world` is one name while `a - b` is two and a minus sign.
        // Without that, `:case snake` over an expression would eat the
        // arithmetic.
        let joined = chars[end] == '-' && end > at && chars.get(end + 1).copied().is_some_and(part);
        if !part(chars[end]) && !joined {
            break;
        }
        end += 1;
    }
    Some(end)
}

/// Splits an identifier into its words, lowercased.
///
/// Three boundaries: a separator, a lower-to-upper step (`helloWorld`), and
/// the last capital of a run before a lowercase one (`HTTPServer` is `http` and
/// `server`, not `httpserver` or `h t t p server`). Digits stay with the word
/// they follow, so `utf8` is one word and not two.
fn words(identifier: &str) -> Vec<String> {
    let chars: Vec<char> = identifier.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();

    for (i, &c) in chars.iter().enumerate() {
        if c == '_' || c == '-' {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
            continue;
        }
        let starts_word = !current.is_empty()
            && c.is_uppercase()
            && (chars[i - 1].is_lowercase()
                || chars[i - 1].is_numeric()
                || chars.get(i + 1).is_some_and(|next| next.is_lowercase()));
        if starts_word {
            out.push(std::mem::take(&mut current));
        }
        current.extend(c.to_lowercase());
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn title(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
        None => String::new(),
    }
}

fn join(words: &[String], style: Style) -> String {
    match style {
        Style::Snake => words.join("_"),
        Style::Kebab => words.join("-"),
        Style::Constant => words.join("_").to_uppercase(),
        Style::Pascal => words.iter().map(|w| title(w)).collect(),
        Style::Camel => words
            .iter()
            .enumerate()
            .map(|(i, w)| if i == 0 { w.clone() } else { title(w) })
            .collect(),
        // Handled before `join` is ever reached.
        Style::Upper | Style::Lower | Style::Title => words.join(" "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(text: &str) -> Vec<String> {
        Style::NAMES.iter().map(|n| convert(text, Style::parse(n).unwrap())).collect()
    }

    #[test]
    fn one_name_in_every_spelling() {
        assert_eq!(
            all("helloWorld"),
            [
                "HELLOWORLD",
                "helloworld",
                "Helloworld",
                "helloWorld",
                "HelloWorld",
                "hello_world",
                "hello-world",
                "HELLO_WORLD",
            ]
        );
    }

    #[test]
    fn every_spelling_is_read_back_the_same_way() {
        for source in ["hello_world", "HelloWorld", "hello-world", "HELLO_WORLD", "helloWorld"] {
            assert_eq!(convert(source, Style::Camel), "helloWorld", "from {source}");
            assert_eq!(convert(source, Style::Snake), "hello_world", "from {source}");
        }
    }

    #[test]
    fn an_acronym_is_one_word_and_a_number_stays_with_its_own() {
        assert_eq!(convert("HTTPServer", Style::Snake), "http_server");
        assert_eq!(convert("parseUTF8Text", Style::Snake), "parse_utf8_text");
        assert_eq!(convert("utf8", Style::Pascal), "Utf8");
    }

    /// Per identifier, because the other reading is what the words literally
    /// say and never what anybody means.
    #[test]
    fn each_identifier_converts_on_its_own() {
        assert_eq!(convert("foo_bar baz_qux", Style::Camel), "fooBar bazQux");
        // *Every* identifier in range, keywords included — which is why you
        // select the name rather than the line.
        assert_eq!(convert("hello_world = 1;", Style::Pascal), "HelloWorld = 1;");
        assert_eq!(convert("let x", Style::Pascal), "Let X");
    }

    #[test]
    fn a_minus_between_words_joins_them_and_one_between_spaces_does_not() {
        assert_eq!(convert("a-b", Style::Snake), "a_b");
        assert_eq!(convert("a - b", Style::Snake), "a - b");
    }

    #[test]
    fn upper_and_lower_leave_everything_else_exactly_where_it_was() {
        assert_eq!(convert("Hello, World! 42", Style::Upper), "HELLO, WORLD! 42");
        assert_eq!(convert("Hello, World! 42", Style::Lower), "hello, world! 42");
        assert_eq!(convert("hello, world", Style::Title), "Hello, World");
    }

    #[test]
    fn the_names_a_style_answers_to() {
        assert_eq!(Style::parse("capital"), Some(Style::Title));
        assert_eq!(Style::parse("screaming"), Some(Style::Constant));
        assert_eq!(Style::parse("dash"), Some(Style::Kebab));
        assert_eq!(Style::parse("nope"), None);
        // Every listed name parses, or `:case` would offer one it cannot take.
        for name in Style::NAMES {
            assert!(Style::parse(name).is_some(), "{name}");
        }
    }
}
