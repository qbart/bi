//! How a file is stored: encoding, BOM, line endings.
//!
//! The rope is always UTF-8 with `\n`. These three are facts about the bytes
//! on disk, and this module is the one boundary where the two meet: [`decode`]
//! on open, [`encode`] on save. Nothing above `Buffer::open`/`save` ever sees
//! a non-UTF-8 byte or a `\r\n`. See `docs/specs/encoding.md`.

use encoding_rs::{Encoding, UTF_8, UTF_8_INIT, UTF_16BE, UTF_16LE, WINDOWS_1252_INIT};

/// What ends a line on disk. In the rope it is always `\n`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FileFormat {
    #[default]
    Unix,
    Dos,
}

impl FileFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            FileFormat::Unix => "unix",
            FileFormat::Dos => "dos",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "unix" | "lf" => Some(FileFormat::Unix),
            "dos" | "crlf" => Some(FileFormat::Dos),
            _ => None,
        }
    }
}

/// The three facts, together because they travel together: detected together
/// at open, consulted together at save, shown together in the status row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Storage {
    pub encoding: &'static Encoding,
    pub bom: bool,
    pub fileformat: FileFormat,
}

impl Default for Storage {
    fn default() -> Self {
        Storage { encoding: UTF_8, bom: false, fileformat: FileFormat::Unix }
    }
}

impl Storage {
    /// Whether there is anything worth a badge in the status row.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// The encoding's name as the status row and `:set` report it.
    pub fn encoding_name(&self) -> String {
        self.encoding.name().to_ascii_lowercase()
    }
}

/// The detection list nobody has configured: UTF-8, then latin1.
///
/// latin1 — windows-1252, which is what the label means on the web and in
/// practice — maps every byte to a char, so the walk cannot fall off the end
/// and opening a file cannot fail. A real UTF-8 file never reaches it, because
/// UTF-8 is tried first and latin1 text essentially never decodes as UTF-8.
pub static DEFAULT_DETECT: [&Encoding; 2] = [&UTF_8_INIT, &WINDOWS_1252_INIT];

/// Everything an open needs to know about storage beyond the path.
///
/// The editor composes one of these from the config's detection list, the
/// project's `.editorconfig` and any `++enc=`/`++ff=` on the command; the
/// default is what an embedder that has said nothing means.
#[derive(Debug, Clone)]
pub struct OpenHow {
    /// Tried in order; an `.editorconfig` `charset` moves to the front.
    pub detect: Vec<&'static Encoding>,
    /// `++enc=` — no detection, this and nothing else.
    pub force_encoding: Option<&'static Encoding>,
    /// `++ff=`, or the project's `end_of_line` — overrides what was detected.
    pub force_fileformat: Option<FileFormat>,
    /// What a file not on disk yet starts as.
    pub new_file: Storage,
}

impl Default for OpenHow {
    fn default() -> Self {
        OpenHow {
            detect: DEFAULT_DETECT.to_vec(),
            force_encoding: None,
            force_fileformat: None,
            new_file: Storage::default(),
        }
    }
}

/// A label as config, `:set fileencoding` or `:e ++enc=` spells it.
///
/// The WHATWG table, which already accepts the vim spellings: `latin1` and
/// `cp1250` are labels for windows-1252 and windows-1250.
pub fn lookup(label: &str) -> Option<&'static Encoding> {
    Encoding::for_label(label.trim().as_bytes())
}

/// `bytes` as text, and what they turned out to be.
///
/// BOM first — a file that declares itself has no need of the list. Otherwise
/// `force` (an explicit `++enc=`) wins, decoded with replacement chars because
/// you said so and refusing would help nobody. Otherwise the first encoding in
/// `detect` that decodes without error; if none does — a configured list can
/// lose its latin1 net — the last entry decodes with replacement chars rather
/// than failing the open.
pub fn decode(
    bytes: &[u8],
    detect: &[&'static Encoding],
    force: Option<&'static Encoding>,
) -> (String, Storage) {
    let (text, encoding, bom) = match Encoding::for_bom(bytes) {
        Some((encoding, len)) if force.is_none() || force == Some(encoding) => {
            (encoding.decode_without_bom_handling(&bytes[len..]).0, encoding, true)
        }
        _ => match force {
            Some(encoding) => (encoding.decode_without_bom_handling(bytes).0, encoding, false),
            None => {
                let (text, encoding) = sniff(bytes, detect);
                (text, encoding, false)
            }
        },
    };
    // UTF-16 without a BOM is a file no other tool will read back; writing
    // one is non-negotiable, so the fact is recorded at the door.
    let bom = bom || is_utf16(encoding);
    let (text, fileformat) = split_line_endings(text.into_owned());
    (text, Storage { encoding, bom, fileformat })
}

fn sniff<'b>(
    bytes: &'b [u8],
    detect: &[&'static Encoding],
) -> (std::borrow::Cow<'b, str>, &'static Encoding) {
    for &encoding in detect {
        if let Some(text) = encoding.decode_without_bom_handling_and_without_replacement(bytes) {
            return (text, encoding);
        }
    }
    let last = detect.last().copied().unwrap_or(UTF_8);
    (last.decode_without_bom_handling(bytes).0, last)
}

fn is_utf16(encoding: &'static Encoding) -> bool {
    encoding == UTF_16LE || encoding == UTF_16BE
}

/// Strips `\r\n` down to `\n` — but only when the whole file agrees.
///
/// A file is `dos` iff it has at least one `\n` and a `\r` before every one of
/// them. A *mixed* file stays `unix` with its stray `\r`s visible in the text,
/// vim-style: calling it `dos` would silently rewrite lines nobody touched on
/// the next save.
fn split_line_endings(text: String) -> (String, FileFormat) {
    let mut newlines = 0usize;
    let mut preceded = 0usize;
    let mut last = '\0';
    for c in text.chars() {
        if c == '\n' {
            newlines += 1;
            if last == '\r' {
                preceded += 1;
            }
        }
        last = c;
    }
    if newlines > 0 && newlines == preceded {
        (text.replace("\r\n", "\n"), FileFormat::Dos)
    } else {
        (text, FileFormat::Unix)
    }
}

/// A char the file's encoding has no bytes for. The save that hit it wrote
/// nothing; `at` is a char index into the buffer's text, for the message to
/// turn into a `line:col`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unencodable {
    pub ch: char,
    pub at: usize,
}

/// The buffer's text as the bytes the file stores: `\r` back before each `\n`
/// if `dos`, the BOM if there is one, the chars as `encoding` spells them.
///
/// `chunks` is the rope's own segmentation — the text arrives in pieces and
/// is never assembled into one `String` on the way out.
///
/// Errs on the first [`Unencodable`] with nothing written; the caller decides
/// what to tell the user. UTF-8 and UTF-16 cannot err — every `char` has a
/// spelling in both.
pub fn encode<'a>(
    chunks: impl Iterator<Item = &'a str>,
    storage: &Storage,
) -> Result<Vec<u8>, Unencodable> {
    let dos = storage.fileformat == FileFormat::Dos;
    let mut out = Vec::new();
    if storage.bom {
        out.extend_from_slice(match storage.encoding {
            e if e == UTF_16LE => &[0xFF, 0xFE][..],
            e if e == UTF_16BE => &[0xFE, 0xFF][..],
            _ => &[0xEF, 0xBB, 0xBF][..],
        });
    }
    if storage.encoding == UTF_8 {
        for chunk in chunks {
            push_expanded(&mut out, chunk, dos);
        }
    } else if is_utf16(storage.encoding) {
        let be = storage.encoding == UTF_16BE;
        let mut units = [0u16; 2];
        let mut push = |c: char, out: &mut Vec<u8>| {
            for unit in c.encode_utf16(&mut units) {
                out.extend_from_slice(&if be { unit.to_be_bytes() } else { unit.to_le_bytes() });
            }
        };
        for chunk in chunks {
            for c in chunk.chars() {
                if c == '\n' && dos {
                    push('\r', &mut out);
                }
                push(c, &mut out);
            }
        }
    } else {
        encode_legacy(chunks, storage.encoding, dos, &mut out)?;
    }
    Ok(out)
}

/// `chunk` with `\r` restored before each `\n`, straight onto `out` as UTF-8.
fn push_expanded(out: &mut Vec<u8>, chunk: &str, dos: bool) {
    if !dos {
        out.extend_from_slice(chunk.as_bytes());
        return;
    }
    let mut rest = chunk;
    while let Some(i) = rest.find('\n') {
        out.extend_from_slice(&rest.as_bytes()[..i]);
        out.extend_from_slice(b"\r\n");
        rest = &rest[i + 1..];
    }
    out.extend_from_slice(rest.as_bytes());
}

/// The `encoding_rs` path — everything that is not UTF-8/16 and can therefore
/// refuse a char.
///
/// The `\r\n` a `dos` file gets back is fed as its own piece and counted as
/// the one `\n` char it is in the rope, so the char index an error carries
/// stays an index into the *buffer's* text, not into what was being written.
fn encode_legacy<'a>(
    chunks: impl Iterator<Item = &'a str>,
    encoding: &'static Encoding,
    dos: bool,
    out: &mut Vec<u8>,
) -> Result<(), Unencodable> {
    let mut encoder = encoding.new_encoder();
    let mut seen = 0usize;
    for chunk in chunks {
        if dos {
            let mut rest = chunk;
            while let Some(i) = rest.find('\n') {
                push_encoded(&mut encoder, &rest[..i], &mut seen, out)?;
                let mut restored = 0; // the "\r\n" is one char of buffer text: the \n
                push_encoded(&mut encoder, "\r\n", &mut restored, out)?;
                seen += 1;
                rest = &rest[i + 1..];
            }
            push_encoded(&mut encoder, rest, &mut seen, out)?;
        } else {
            push_encoded(&mut encoder, chunk, &mut seen, out)?;
        }
    }
    flush(&mut encoder, out);
    Ok(())
}

fn push_encoded(
    encoder: &mut encoding_rs::Encoder,
    piece: &str,
    seen: &mut usize,
    out: &mut Vec<u8>,
) -> Result<(), Unencodable> {
    let mut src = piece;
    loop {
        if src.is_empty() {
            break;
        }
        let need = encoder
            .max_buffer_length_from_utf8_without_replacement(src.len())
            .unwrap_or(src.len() * 4 + 16);
        let start = out.len();
        out.resize(start + need.max(16), 0);
        let (result, read, written) =
            encoder.encode_from_utf8_without_replacement(src, &mut out[start..], false);
        out.truncate(start + written);
        match result {
            encoding_rs::EncoderResult::InputEmpty => {
                *seen += src.chars().count();
                break;
            }
            encoding_rs::EncoderResult::OutputFull => {
                *seen += src[..read].chars().count();
                src = &src[read..];
            }
            encoding_rs::EncoderResult::Unmappable(ch) => {
                // `read` has consumed the unmappable char; step back over it.
                let at = *seen + src[..read].chars().count() - 1;
                return Err(Unencodable { ch, at });
            }
        }
    }
    Ok(())
}

/// Tells the encoder the text is over. Only ISO-2022-JP has bytes to add at
/// the end, but the contract is the contract.
fn flush(encoder: &mut encoding_rs::Encoder, out: &mut Vec<u8>) {
    let start = out.len();
    out.resize(start + 16, 0);
    let (_, _, written) = encoder.encode_from_utf8_without_replacement("", &mut out[start..], true);
    out.truncate(start + written);
}

#[cfg(test)]
mod tests {
    use super::*;
    use encoding_rs::WINDOWS_1252;

    fn detect() -> Vec<&'static Encoding> {
        DEFAULT_DETECT.to_vec()
    }

    fn round_trip(bytes: &[u8]) -> (String, Storage, Vec<u8>) {
        let (text, storage) = decode(bytes, &detect(), None);
        let back = encode(std::iter::once(text.as_str()), &storage).expect("encodable");
        (text, storage, back)
    }

    #[test]
    fn utf8_stays_utf8_and_byte_identical() {
        let (text, storage, back) = round_trip("żółć\n".as_bytes());
        assert_eq!(text, "żółć\n");
        assert!(storage.is_default());
        assert_eq!(back, "żółć\n".as_bytes());
    }

    #[test]
    fn latin1_is_the_net_under_utf8() {
        // "café" in latin1 — 0xE9 is no UTF-8.
        let bytes = b"caf\xE9\n";
        let (text, storage, back) = round_trip(bytes);
        assert_eq!(text, "café\n");
        assert_eq!(storage.encoding, WINDOWS_1252);
        assert_eq!(back, bytes);
    }

    #[test]
    fn a_bom_wins_over_the_list_and_survives_the_round_trip() {
        let bytes = b"\xEF\xBB\xBFhi\n";
        let (text, storage, back) = round_trip(bytes);
        assert_eq!(text, "hi\n", "the BOM is storage, not text");
        assert!(storage.bom);
        assert_eq!(storage.encoding, UTF_8);
        assert_eq!(back, bytes);
    }

    #[test]
    fn utf16le_decodes_by_bom_and_encodes_by_hand() {
        let bytes = b"\xFF\xFEh\x00i\x00\n\x00";
        let (text, storage, back) = round_trip(bytes);
        assert_eq!(text, "hi\n");
        assert_eq!(storage.encoding, UTF_16LE);
        assert!(storage.bom);
        assert_eq!(back, bytes);
    }

    #[test]
    fn utf16be_round_trips_too() {
        let bytes = b"\xFE\xFF\x00h\x00i\x00\n";
        let (text, storage, back) = round_trip(bytes);
        assert_eq!(text, "hi\n");
        assert_eq!(storage.encoding, UTF_16BE);
        assert_eq!(back, bytes);
    }

    #[test]
    fn crlf_throughout_is_dos_and_the_rope_never_sees_the_cr() {
        let bytes = b"one\r\ntwo\r\n";
        let (text, storage, back) = round_trip(bytes);
        assert_eq!(text, "one\ntwo\n");
        assert_eq!(storage.fileformat, FileFormat::Dos);
        assert_eq!(back, bytes);
    }

    #[test]
    fn mixed_endings_stay_unix_and_keep_their_strays() {
        let bytes = b"one\r\ntwo\n";
        let (text, storage, back) = round_trip(bytes);
        assert_eq!(text, "one\r\ntwo\n", "the stray \\r is text, visible");
        assert_eq!(storage.fileformat, FileFormat::Unix);
        assert_eq!(back, bytes);
    }

    #[test]
    fn a_lone_cr_is_a_char_not_a_line_ending() {
        let (text, storage, _) = round_trip(b"a\rb\n");
        assert_eq!(text, "a\rb\n");
        assert_eq!(storage.fileformat, FileFormat::Unix);
    }

    #[test]
    fn force_overrides_detection() {
        // Valid UTF-8, but the user says latin1.
        let bytes = "ż".as_bytes(); // 0xC5 0xBC — latin1 reads two chars
        let (text, storage) = decode(bytes, &detect(), lookup("latin1"));
        assert_eq!(storage.encoding, WINDOWS_1252);
        assert_eq!(text.chars().count(), 2);
    }

    #[test]
    fn force_agreeing_with_the_bom_still_strips_it() {
        let (text, storage) = decode(b"\xEF\xBB\xBFhi", &detect(), Some(UTF_8));
        assert_eq!(text, "hi");
        assert!(storage.bom);
    }

    #[test]
    fn an_exhausted_list_falls_back_to_its_last_entry_lossily() {
        let bytes = b"\xFF\xFF";
        let only_utf8: Vec<&'static Encoding> = vec![UTF_8];
        let (text, storage) = decode(bytes, &only_utf8, None);
        assert_eq!(storage.encoding, UTF_8);
        assert_eq!(text, "\u{FFFD}\u{FFFD}", "replacement chars, not a refused open");
    }

    #[test]
    fn unencodable_names_the_char_and_where_it_is() {
        let storage = Storage { encoding: WINDOWS_1252, bom: false, fileformat: FileFormat::Unix };
        let err = encode(std::iter::once("ok\nżle"), &storage).unwrap_err();
        assert_eq!(err.ch, 'ż');
        assert_eq!(err.at, 3, "a char index into the buffer's text");
    }

    #[test]
    fn unencodable_position_is_in_buffer_chars_even_in_a_dos_file() {
        let storage = Storage { encoding: WINDOWS_1252, bom: false, fileformat: FileFormat::Dos };
        let err = encode(std::iter::once("ok\nżle"), &storage).unwrap_err();
        assert_eq!(err.at, 3, "the \\r the file gets back does not shift it");
    }

    #[test]
    fn dos_encoding_restores_the_cr_in_legacy_encodings_too() {
        let storage = Storage { encoding: WINDOWS_1252, bom: false, fileformat: FileFormat::Dos };
        let bytes = encode(std::iter::once("a\nb\n"), &storage).unwrap();
        assert_eq!(bytes, b"a\r\nb\r\n");
    }

    #[test]
    fn chunk_boundaries_do_not_change_the_bytes() {
        let storage = Storage { encoding: WINDOWS_1252, bom: false, fileformat: FileFormat::Dos };
        let whole = encode(std::iter::once("a\nbé\n"), &storage).unwrap();
        let pieces = encode(["a\nb", "é", "\n"].into_iter(), &storage).unwrap();
        assert_eq!(whole, pieces);
    }

    #[test]
    fn labels_speak_vim() {
        assert_eq!(lookup("latin1"), Some(WINDOWS_1252));
        assert_eq!(lookup("cp1250").map(|e| e.name()), Some("windows-1250"));
        assert_eq!(lookup("utf-8"), Some(UTF_8));
        assert!(lookup("no-such-encoding").is_none());
    }

    #[test]
    fn utf16_gets_its_bom_even_unasked() {
        let (_, storage) = decode(b"h\x00i\x00", &detect(), lookup("utf-16le"));
        assert!(storage.bom, "a UTF-16 file without a BOM is unreadable by convention");
    }

    #[test]
    fn fileformat_parses_both_spellings() {
        assert_eq!(FileFormat::parse("unix"), Some(FileFormat::Unix));
        assert_eq!(FileFormat::parse("crlf"), Some(FileFormat::Dos));
        assert_eq!(FileFormat::parse("mac"), None);
    }
}
