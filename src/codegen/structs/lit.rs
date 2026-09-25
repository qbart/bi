//! Literals: the schema's JSON values spelled for each language. Floats
//! that stay floats, strings escaped the way the target reads them,
//! colours as channels, a range as the doc comment's `0..999`.

use serde_json::Value;

use super::Lang;

/// A float literal that every language types as a float: `1` → `1.0`,
/// with an `f` suffix for C and C++ singles.
pub fn float(v: f64, single: bool, lang: Lang) -> String {
    let mut s = if v == v.trunc() && v.abs() < 1e16 {
        if v.is_sign_negative() && v == 0.0 { "-0.0".to_string() } else { format!("{v:.1}") }
    } else {
        format!("{v}")
    };
    if !s.contains('.') && !s.contains('e') {
        s.push_str(".0");
    }
    if single && matches!(lang, Lang::C | Lang::Cpp) {
        s.push('f');
    }
    s
}

/// A number as the float its field wants, from the JSON number.
pub fn float_of(v: &Value, single: bool, lang: Lang) -> String {
    float(v.as_f64().unwrap_or(0.0), single, lang)
}

/// An integer literal, exact: the JSON number's own digits.
pub fn int(v: &Value) -> String {
    match v {
        Value::Number(n) => n.to_string(),
        _ => "0".to_string(),
    }
}

/// `s` quoted and escaped for `lang`. UTF-8 passes through; `"`, `\` and
/// every control character are escaped the way the language reads them.
pub fn string(s: &str, lang: Lang) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => match lang {
                Lang::Rust => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
                Lang::Go => out.push_str(&format!("\\u{:04x}", c as u32)),
                Lang::Lua => out.push_str(&format!("\\{:03}", c as u32)),
                Lang::C | Lang::Cpp | Lang::C3 => {
                    out.push_str(&format!("\\x{:02x}", c as u32));
                    // A hex digit after `\x..` would be eaten by it.
                    if chars.get(i + 1).is_some_and(|n| n.is_ascii_hexdigit()) {
                        out.push_str("\"\"");
                    }
                }
            },
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `#rrggbb` or `#rrggbbaa` as channels, alpha `255` when six digits.
/// The value has been through the schema's check, so an odd one is
/// black rather than an error.
pub fn colour(s: &str) -> (u8, u8, u8, u8) {
    let hex = s.trim_start_matches('#');
    let ch = |i: usize| u8::from_str_radix(hex.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0);
    let a = if hex.len() >= 8 { ch(6) } else { 255 };
    (ch(0), ch(2), ch(4), a)
}

/// `0..999`, `1..`, `..5`, `0..1 step 0.01`; `None` when none is set.
pub fn range(min: Option<f64>, max: Option<f64>, step: Option<f64>) -> Option<String> {
    if min.is_none() && max.is_none() && step.is_none() {
        return None;
    }
    let mut s = String::new();
    if min.is_some() || max.is_some() {
        if let Some(m) = min {
            s.push_str(&compact(m));
        }
        s.push_str("..");
        if let Some(m) = max {
            s.push_str(&compact(m));
        }
    }
    if let Some(st) = step {
        if !s.is_empty() {
            s.push(' ');
        }
        s.push_str(&format!("step {}", compact(st)));
    }
    Some(s)
}

/// A number the way a doc comment reads it: `10`, `0.05`.
pub fn compact(n: f64) -> String {
    if n == n.trunc() && n.abs() < 1e16 { format!("{}", n as i64) } else { format!("{n}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_stay_floats() {
        assert_eq!(float(1.0, false, Lang::Rust), "1.0");
        assert_eq!(float(1.0, true, Lang::Cpp), "1.0f");
        assert_eq!(float(1.0, true, Lang::Rust), "1.0");
        assert_eq!(float(0.05, false, Lang::Go), "0.05");
        assert_eq!(float(-0.0, false, Lang::Rust), "-0.0");
        assert_eq!(float(2.5, true, Lang::C), "2.5f");
        assert_eq!(float(1e20, false, Lang::Rust), "100000000000000000000.0");
        assert_eq!(int(&serde_json::json!(18446744073709551615u64)), "18446744073709551615");
        assert_eq!(int(&serde_json::json!(-7)), "-7");
    }

    #[test]
    fn strings_are_escaped_per_language() {
        assert_eq!(string("a\"b\\\n", Lang::Rust), "\"a\\\"b\\\\\\n\"");
        assert_eq!(string("é", Lang::Go), "\"é\"");
        assert_eq!(string("\u{1}", Lang::Rust), "\"\\u{1}\"");
        assert_eq!(string("\u{1}", Lang::Go), "\"\\u0001\"");
        assert_eq!(string("\u{1}", Lang::Lua), "\"\\001\"");
        assert_eq!(string("\u{1}a", Lang::C), "\"\\x01\"\"a\"");
        assert_eq!(string("\u{1}z", Lang::Cpp), "\"\\x01z\"");
        assert_eq!(string("t\tab", Lang::C3), "\"t\\tab\"");
    }

    #[test]
    fn colours_and_ranges() {
        assert_eq!(colour("#c8c8c8"), (200, 200, 200, 255));
        assert_eq!(colour("#ff000080"), (255, 0, 0, 128));
        assert_eq!(range(Some(0.), Some(999.), None).as_deref(), Some("0..999"));
        assert_eq!(range(Some(1.), None, None).as_deref(), Some("1.."));
        assert_eq!(range(None, Some(5.), None).as_deref(), Some("..5"));
        assert_eq!(range(Some(0.), Some(1.), Some(0.01)).as_deref(), Some("0..1 step 0.01"));
        assert_eq!(range(None, None, Some(2.)).as_deref(), Some("step 2"));
        assert_eq!(range(None, None, None), None);
    }
}
