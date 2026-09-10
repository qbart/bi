//! Text, spelled differently: `:base64e`, `:hexe`, `:urle`, `:jsonfmt`,
//! `:md5` and their kin.
//!
//! Text in, text out or an error, and no knowledge of a buffer anywhere in
//! it. One enum rather than a function per command, so that the next
//! spelling is an arm here and nothing new in the editor.
//!
//! See `docs/specs/transform.md`.

use base64::Engine;
use base64::engine::DecodePaddingMode;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig, STANDARD};
use md5::Digest;

/// Which respelling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transform {
    /// `:base64e` — the standard alphabet, padded, on one line.
    Base64Encode,
    /// `:base64d` — lenient on the way in, strict on the way out.
    Base64Decode,
    /// `:hexe` — the UTF-8 bytes as lowercase hex, no separators.
    HexEncode,
    /// `:hexd` — either case, whitespace ignored.
    HexDecode,
    /// `:urle` — percent-encoding; unreserved characters stay.
    UrlEncode,
    /// `:urld` — `%XX` back to bytes; a `+` is a `+`.
    UrlDecode,
    /// `:jsonfmt` — pretty-printed, one indent unit per level. The unit is
    /// the buffer's, filled in by the editor before the transform runs.
    JsonFormat { indent: String },
    /// `:jsonmin` — every insignificant byte gone.
    JsonMinify,
    /// `:md5` — the digest of the UTF-8 bytes, lowercase hex.
    Md5,
    /// `:sha256` — likewise.
    Sha256,
}

/// Every command name here, for the range whitelist.
pub const NAMES: &[&str] =
    &["base64e", "base64d", "hexe", "hexd", "urle", "urld", "jsonfmt", "jsonmin", "md5", "sha256"];

/// Standard alphabet, padding optional: what makes a blob pasted from
/// anywhere decode.
const LENIENT: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

impl Transform {
    /// The transform a command name asks for, or `None` for a name that is
    /// not one.
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "base64e" => Transform::Base64Encode,
            "base64d" => Transform::Base64Decode,
            "hexe" => Transform::HexEncode,
            "hexd" => Transform::HexDecode,
            "urle" => Transform::UrlEncode,
            "urld" => Transform::UrlDecode,
            "jsonfmt" => Transform::JsonFormat { indent: "  ".into() },
            "jsonmin" => Transform::JsonMinify,
            "md5" => Transform::Md5,
            "sha256" => Transform::Sha256,
            _ => return None,
        })
    }

    /// The same, with the indent unit `:jsonfmt` should use. A no-op for
    /// every other arm.
    pub fn with_indent(self, unit: &str) -> Self {
        match self {
            Transform::JsonFormat { .. } => Transform::JsonFormat { indent: unit.into() },
            other => other,
        }
    }

    /// The deed, past tense: what the report says.
    pub fn done(&self) -> &'static str {
        match self {
            Transform::Base64Encode | Transform::HexEncode | Transform::UrlEncode => "encoded",
            Transform::Base64Decode | Transform::HexDecode | Transform::UrlDecode => "decoded",
            Transform::JsonFormat { .. } => "formatted",
            Transform::JsonMinify => "minified",
            Transform::Md5 | Transform::Sha256 => "hashed",
        }
    }

    /// The deed, infinitive: `nothing to encode`.
    pub fn deed(&self) -> &'static str {
        match self {
            Transform::Base64Encode | Transform::HexEncode | Transform::UrlEncode => "encode",
            Transform::Base64Decode | Transform::HexDecode | Transform::UrlDecode => "decode",
            Transform::JsonFormat { .. } => "format",
            Transform::JsonMinify => "minify",
            Transform::Md5 | Transform::Sha256 => "hash",
        }
    }

    pub fn apply(&self, text: &str) -> Result<String, String> {
        match self {
            Transform::Base64Encode => Ok(STANDARD.encode(text.as_bytes())),
            Transform::Base64Decode => base64_decode(text),
            Transform::HexEncode => Ok(hex(text.as_bytes())),
            Transform::HexDecode => hex_decode(text),
            Transform::UrlEncode => Ok(url_encode(text)),
            Transform::UrlDecode => url_decode(text),
            Transform::JsonFormat { indent } => json_format(text, indent),
            Transform::JsonMinify => json_minify(text),
            Transform::Md5 => Ok(hex(&md5::Md5::digest(text.as_bytes()))),
            Transform::Sha256 => Ok(hex(&sha2::Sha256::digest(text.as_bytes()))),
        }
    }
}

/// Whitespace anywhere is ignored — a wrapped blob is one blob — and the
/// URL-safe alphabet is read as the standard one. What comes out must be
/// text: the buffer holds nothing else.
fn base64_decode(text: &str) -> Result<String, String> {
    let mut compact = String::with_capacity(text.len());
    // Offsets in the message are into the text as written, not as compacted.
    let mut offsets = Vec::with_capacity(text.len());
    for (offset, c) in text.char_indices() {
        if c.is_whitespace() {
            continue;
        }
        compact.push(match c {
            '-' => '+',
            '_' => '/',
            other => other,
        });
        offsets.push(offset);
    }
    // A foreign character is named before the length is judged: the crate
    // checks length first, and "the length is off" is the wrong thing to say
    // about a `*` in the middle.
    if let Some((at, c)) =
        compact.chars().enumerate().find(|(_, c)| !c.is_ascii_alphanumeric() && !"+/=".contains(*c))
    {
        return Err(format!("not base64: `{c}` at {}", offsets[at]));
    }
    let bytes = LENIENT.decode(compact.as_bytes()).map_err(|error| match error {
        base64::DecodeError::InvalidByte(at, _) => {
            let offset = offsets.get(at).copied().unwrap_or(at);
            let c = text[offset..].chars().next().unwrap_or('?');
            format!("not base64: `{c}` at {offset}")
        }
        base64::DecodeError::InvalidLastSymbol(at, _) => {
            format!("not base64: a stray symbol at {}", offsets.get(at).copied().unwrap_or(at))
        }
        base64::DecodeError::InvalidLength(_) => "not base64: the length is off".into(),
        base64::DecodeError::InvalidPadding => "not base64: the padding is off".into(),
    })?;
    text_of(bytes)
}

fn text_of(bytes: Vec<u8>) -> Result<String, String> {
    String::from_utf8(bytes).map_err(|_| "decoded bytes are not UTF-8".to_string())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Either case, whitespace ignored. An odd count of digits is a half byte,
/// which is not a byte.
fn hex_decode(text: &str) -> Result<String, String> {
    let mut digits = Vec::with_capacity(text.len());
    for (offset, c) in text.char_indices() {
        if c.is_whitespace() {
            continue;
        }
        match c.to_digit(16) {
            Some(d) => digits.push(d as u8),
            None => return Err(format!("not hex: `{c}` at {offset}")),
        }
    }
    if digits.len() % 2 == 1 {
        return Err("not hex: an odd number of digits".into());
    }
    text_of(digits.chunks(2).map(|pair| pair[0] << 4 | pair[1]).collect())
}

/// RFC 3986's unreserved set stays; everything else, including a space,
/// is `%XX` per UTF-8 byte.
fn url_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// `%XX` back to bytes. A `+` stays a `+`: reading it as a space is the
/// form-encoding dialect, and a query string is not the only place a URL
/// escape turns up.
fn url_decode(text: &str) -> Result<String, String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'%' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        let digit = |at: usize| bytes.get(at).and_then(|b| (*b as char).to_digit(16));
        match (digit(i + 1), digit(i + 2)) {
            (Some(hi), Some(lo)) => out.push((hi << 4 | lo) as u8),
            _ => return Err(format!("not a URL escape: `%` at {i} without two hex digits")),
        }
        i += 3;
    }
    text_of(out)
}

fn json_value(text: &str) -> Result<serde_json::Value, String> {
    serde_json::from_str(text).map_err(|error| format!("not JSON: {error}"))
}

fn json_format(text: &str, indent: &str) -> Result<String, String> {
    let value = json_value(text)?;
    let formatter = serde_json::ser::PrettyFormatter::with_indent(indent.as_bytes());
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut out, formatter);
    serde::Serialize::serialize(&value, &mut ser).map_err(|error| format!("not JSON: {error}"))?;
    text_of(out)
}

fn json_minify(text: &str) -> Result<String, String> {
    let value = json_value(text)?;
    serde_json::to_string(&value).map_err(|error| format!("not JSON: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(name: &str, text: &str) -> Result<String, String> {
        Transform::parse(name).unwrap().apply(text)
    }

    #[test]
    fn every_name_parses_and_a_stranger_does_not() {
        for name in NAMES {
            assert!(Transform::parse(name).is_some(), "{name}");
        }
        assert_eq!(Transform::parse("rot13"), None);
    }

    #[test]
    fn base64_encoding_is_standard_padded_and_on_one_line() {
        assert_eq!(run("base64e", "hello\nworld").unwrap(), "aGVsbG8Kd29ybGQ=");
        assert_eq!(run("base64e", "").unwrap(), "");
        let long = "x".repeat(200);
        assert!(!run("base64e", &long).unwrap().contains('\n'));
        // The standard alphabet: `+` and `/`, not `-` and `_`.
        assert_eq!(run("base64e", "??>???").unwrap(), "Pz8+Pz8/");
    }

    #[test]
    fn base64_decoding_reads_whitespace_both_alphabets_and_missing_padding() {
        assert_eq!(run("base64d", "aGVs\nbG8K d29y\tbGQ=").unwrap(), "hello\nworld");
        assert_eq!(run("base64d", "aGVsbG8Kd29ybGQ").unwrap(), "hello\nworld");
        // `??>` is `Pz8+` in the standard alphabet and `Pz8-` in the URL-safe
        // one; `???` is `Pz8/` and `Pz8_`.
        assert_eq!(run("base64d", "Pz8-Pz8_").unwrap(), "??>???");
    }

    #[test]
    fn a_bad_base64_character_names_itself_and_where_it_sits() {
        assert_eq!(
            run("base64d", "aGVs bG8*").unwrap_err(),
            "not base64: `*` at 8",
            "the offset is into the text as written, whitespace counted"
        );
    }

    #[test]
    fn bytes_that_are_not_text_are_refused() {
        // 0xff 0xfe: not UTF-8 by any reading.
        assert_eq!(run("base64d", "//4=").unwrap_err(), "decoded bytes are not UTF-8");
        assert_eq!(run("hexd", "fffe").unwrap_err(), "decoded bytes are not UTF-8");
        assert_eq!(run("urld", "%FF%FE").unwrap_err(), "decoded bytes are not UTF-8");
    }

    #[test]
    fn hex_is_lowercase_out_and_either_case_in() {
        assert_eq!(run("hexe", "Hi\n").unwrap(), "48690a");
        assert_eq!(run("hexd", "48 69\n0A").unwrap(), "Hi\n");
        assert_eq!(run("hexd", "4g").unwrap_err(), "not hex: `g` at 1");
        assert_eq!(run("hexd", "486").unwrap_err(), "not hex: an odd number of digits");
    }

    #[test]
    fn url_escapes_everything_but_the_unreserved_set() {
        assert_eq!(run("urle", "a b/c?d=é~_.-").unwrap(), "a%20b%2Fc%3Fd%3D%C3%A9~_.-");
        assert_eq!(run("urld", "a%20b%2fc+d").unwrap(), "a b/c+d", "`+` stays a `+`");
        assert_eq!(
            run("urld", "ab%2").unwrap_err(),
            "not a URL escape: `%` at 2 without two hex digits"
        );
    }

    #[test]
    fn json_formats_with_the_given_unit_and_keeps_key_order() {
        let text = r#"{"b":1,"a":[true,null],"c":{}}"#;
        assert_eq!(
            run("jsonfmt", text).unwrap(),
            "{\n  \"b\": 1,\n  \"a\": [\n    true,\n    null\n  ],\n  \"c\": {}\n}"
        );
        let tabbed = Transform::parse("jsonfmt").unwrap().with_indent("\t");
        assert!(tabbed.apply(text).unwrap().starts_with("{\n\t\"b\": 1,"));
        assert_eq!(
            run("jsonmin", "{ \"b\" : 1 ,\n \"a\" : [ 1, 2 ] }").unwrap(),
            r#"{"b":1,"a":[1,2]}"#
        );
        assert!(run("jsonfmt", "{oops}").unwrap_err().starts_with("not JSON: "));
    }

    #[test]
    fn digests_are_lowercase_hex() {
        assert_eq!(run("md5", "hello").unwrap(), "5d41402abc4b2a76b9719d911017c592");
        assert_eq!(
            run("sha256", "hello").unwrap(),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn the_pairs_round_trip() {
        let text = "zażółć gęślą jaźń\n\ttabs\r\n %&+";
        for (e, d) in [("base64e", "base64d"), ("hexe", "hexd"), ("urle", "urld")] {
            assert_eq!(run(d, &run(e, text).unwrap()).unwrap(), text, "{e}/{d}");
        }
    }
}
