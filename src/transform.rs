//! Text, spelled differently: `:base64e` and `:base64d`.
//!
//! Text in, text out or an error, and no knowledge of a buffer anywhere in
//! it. One enum rather than a function per command, so that the next
//! spelling — hex, a URL escape, a JSON string — is an arm here and nothing
//! new in the editor.
//!
//! See `docs/specs/transform.md`.

use base64::Engine;
use base64::engine::DecodePaddingMode;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig, STANDARD};

/// Which respelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transform {
    /// `:base64e` — the standard alphabet, padded, on one line.
    Base64Encode,
    /// `:base64d` — lenient on the way in, strict on the way out.
    Base64Decode,
}

/// Standard alphabet, padding optional: what makes a blob pasted from
/// anywhere decode.
const LENIENT: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

impl Transform {
    /// The command's name, for messages.
    pub fn name(self) -> &'static str {
        match self {
            Transform::Base64Encode => "base64e",
            Transform::Base64Decode => "base64d",
        }
    }

    /// What the report calls the deed.
    pub fn verb(self) -> &'static str {
        match self {
            Transform::Base64Encode => "encoded",
            Transform::Base64Decode => "decoded",
        }
    }

    pub fn apply(self, text: &str) -> Result<String, String> {
        match self {
            Transform::Base64Encode => Ok(STANDARD.encode(text.as_bytes())),
            Transform::Base64Decode => decode(text),
        }
    }
}

/// Whitespace anywhere is ignored — a wrapped blob is one blob — and the
/// URL-safe alphabet is read as the standard one. What comes out must be
/// text: the buffer holds nothing else.
fn decode(text: &str) -> Result<String, String> {
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
    String::from_utf8(bytes).map_err(|_| "decoded bytes are not UTF-8".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_is_standard_padded_and_on_one_line() {
        assert_eq!(Transform::Base64Encode.apply("hello\nworld").unwrap(), "aGVsbG8Kd29ybGQ=");
        assert_eq!(Transform::Base64Encode.apply("").unwrap(), "");
        let long = "x".repeat(200);
        assert!(!Transform::Base64Encode.apply(&long).unwrap().contains('\n'));
        // The standard alphabet: `+` and `/`, not `-` and `_`.
        assert_eq!(Transform::Base64Encode.apply("\u{fb}\u{ff}").unwrap(), "w7vDvw==");
    }

    #[test]
    fn decoding_reads_whitespace_both_alphabets_and_missing_padding() {
        assert_eq!(Transform::Base64Decode.apply("aGVs\nbG8K d29y\tbGQ=").unwrap(), "hello\nworld");
        assert_eq!(Transform::Base64Decode.apply("aGVsbG8Kd29ybGQ").unwrap(), "hello\nworld");
        assert_eq!(Transform::Base64Decode.apply("w7vDvw").unwrap(), "\u{fb}\u{ff}");
        // `??>` is `Pz8+` in the standard alphabet and `Pz8-` in the URL-safe
        // one; `???` is `Pz8/` and `Pz8_`.
        assert_eq!(Transform::Base64Decode.apply("Pz8-Pz8_").unwrap(), "??>???");
    }

    #[test]
    fn a_bad_character_names_itself_and_where_it_sits() {
        assert_eq!(
            Transform::Base64Decode.apply("aGVs bG8*").unwrap_err(),
            "not base64: `*` at 8",
            "the offset is into the text as written, whitespace counted"
        );
    }

    #[test]
    fn bytes_that_are_not_text_are_refused() {
        // 0xff 0xfe: not UTF-8 by any reading.
        assert_eq!(
            Transform::Base64Decode.apply("//4=").unwrap_err(),
            "decoded bytes are not UTF-8"
        );
    }

    #[test]
    fn the_pair_round_trips() {
        let text = "zażółć gęślą jaźń\n\ttabs\r\n";
        let encoded = Transform::Base64Encode.apply(text).unwrap();
        assert_eq!(Transform::Base64Decode.apply(&encoded).unwrap(), text);
    }
}
