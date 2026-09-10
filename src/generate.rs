//! Text that is known before it is typed: `:uuid4`, `:date`, `:password`
//! and their kin.
//!
//! Nothing in, text out. One enum rather than a command per kind, so that
//! the next generated thing is an arm here and nothing new in the editor.
//!
//! See `docs/specs/generate.md`.

/// What to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Generator {
    /// `:uuid4` — random.
    Uuid4,
    /// `:uuid7` — time-ordered; two made in order sort in order.
    Uuid7,
    /// `:uuidzero` — the nil uuid.
    UuidZero,
    /// `:ulid` — time-ordered, Crockford base32, 26 characters.
    Ulid,
    /// `:nanoid` — 21 characters of `A-Za-z0-9_-`.
    NanoId,
    /// `:epoch` — seconds since 1970, UTC.
    Epoch,
    /// `:date [fmt]` — today, local time, `%Y-%m-%d` unless told otherwise.
    Date(Option<String>),
    /// `:time [fmt]` — now, local time, `%H:%M:%S` unless told otherwise.
    Time(Option<String>),
    /// `:lorem [n]` — that many paragraphs of the classic filler.
    Lorem(usize),
    /// `:password [len]` — that many characters, letters digits and symbols.
    Password(usize),
}

/// Every command name here, for the range whitelist.
pub const NAMES: &[&str] =
    &["uuid4", "uuid7", "uuidzero", "ulid", "nanoid", "epoch", "date", "time", "lorem", "password"];

const DATE: &str = "%Y-%m-%d";
const TIME: &str = "%H:%M:%S";
const PASSWORD_LEN: usize = 24;
/// Letters, digits, and symbols no shell, URL or JSON string chokes on.
const PASSWORD_ALPHABET: &[u8] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!@#$%^&*-_=+";
const NANOID_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-";

const LOREM: &[&str] = &[
    "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.",
    "Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat.",
    "Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur.",
    "Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.",
    "Sed ut perspiciatis unde omnis iste natus error sit voluptatem accusantium doloremque laudantium.",
    "Nemo enim ipsam voluptatem quia voluptas sit aspernatur aut odit aut fugit, sed quia consequuntur magni dolores eos qui ratione voluptatem sequi nesciunt.",
];

impl Generator {
    /// The generator a command name and its argument ask for. `None` for a
    /// name that is not one; `Some(Err)` for an argument that does not fit.
    pub fn parse(name: &str, arg: &str) -> Option<Result<Self, String>> {
        let arg = arg.trim();
        let bare = |what: Generator| match arg.is_empty() {
            true => Ok(what),
            false => Err(format!("{name} takes no argument")),
        };
        let count = |default: usize, example: &str| -> Result<usize, String> {
            if arg.is_empty() {
                return Ok(default);
            }
            match arg.parse::<usize>() {
                Ok(n) if n > 0 => Ok(n),
                _ => Err(format!("{name} how many? `:{name} {example}`")),
            }
        };
        let format = || (!arg.is_empty()).then(|| arg.to_string());
        Some(match name {
            "uuid4" => bare(Generator::Uuid4),
            "uuid7" => bare(Generator::Uuid7),
            "uuidzero" => bare(Generator::UuidZero),
            "ulid" => bare(Generator::Ulid),
            "nanoid" => bare(Generator::NanoId),
            "epoch" => bare(Generator::Epoch),
            "date" => Ok(Generator::Date(format())),
            "time" => Ok(Generator::Time(format())),
            "lorem" => count(1, "3").map(Generator::Lorem),
            "password" => count(PASSWORD_LEN, "16").map(Generator::Password),
            _ => return None,
        })
    }

    /// The command's name, for messages.
    pub fn name(&self) -> &'static str {
        match self {
            Generator::Uuid4 => "uuid4",
            Generator::Uuid7 => "uuid7",
            Generator::UuidZero => "uuidzero",
            Generator::Ulid => "ulid",
            Generator::NanoId => "nanoid",
            Generator::Epoch => "epoch",
            Generator::Date(_) => "date",
            Generator::Time(_) => "time",
            Generator::Lorem(_) => "lorem",
            Generator::Password(_) => "password",
        }
    }

    /// A fresh value. Called once per cursor, which is what makes three
    /// cursors three different ids. The only way this fails is a strftime
    /// format that is not one.
    pub fn generate(&self) -> Result<String, String> {
        Ok(match self {
            Generator::Uuid4 => uuid::Uuid::new_v4().hyphenated().to_string(),
            Generator::Uuid7 => uuid::Uuid::now_v7().hyphenated().to_string(),
            Generator::UuidZero => uuid::Uuid::nil().hyphenated().to_string(),
            Generator::Ulid => ulid::Ulid::from_datetime(std::time::SystemTime::now()).to_string(),
            Generator::NanoId => random(NANOID_ALPHABET, 21),
            Generator::Epoch => jiff::Timestamp::now().as_second().to_string(),
            Generator::Date(fmt) => strftime(fmt.as_deref().unwrap_or(DATE))?,
            Generator::Time(fmt) => strftime(fmt.as_deref().unwrap_or(TIME))?,
            Generator::Lorem(n) => lorem(*n),
            Generator::Password(len) => random(PASSWORD_ALPHABET, *len),
        })
    }
}

fn strftime(fmt: &str) -> Result<String, String> {
    jiff::fmt::strtime::format(fmt, &jiff::Zoned::now())
        .map_err(|error| format!("bad format `{fmt}`: {error}"))
}

/// `len` characters drawn uniformly from `alphabet`, each on its own.
fn random(alphabet: &[u8], len: usize) -> String {
    (0..len).map(|_| alphabet[rand::random_range(0..alphabet.len())] as char).collect()
}

/// `n` paragraphs, each starting one sentence further into the corpus so
/// that no two in a row read the same. Deterministic: filler that changes
/// under you is filler you cannot diff.
fn lorem(n: usize) -> String {
    (0..n)
        .map(|k| {
            (0..LOREM.len()).map(|i| LOREM[(i + k) % LOREM.len()]).collect::<Vec<_>>().join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(name: &str, arg: &str) -> String {
        Generator::parse(name, arg).unwrap().unwrap().generate().unwrap()
    }

    fn is_canonical_uuid(id: &str, version: char) -> bool {
        id.len() == 36
            && id.chars().all(|c| c == '-' || c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            && id.chars().nth(14) == Some(version)
    }

    #[test]
    fn every_name_parses_and_a_stranger_does_not() {
        for name in NAMES {
            assert!(Generator::parse(name, "").is_some(), "{name}");
        }
        assert!(Generator::parse("rot13", "").is_none());
    }

    #[test]
    fn uuid4_is_canonical_and_random() {
        let (a, b) = (make("uuid4", ""), make("uuid4", ""));
        assert!(is_canonical_uuid(&a, '4'), "{a}");
        assert_ne!(a, b);
    }

    #[test]
    fn uuid7_is_canonical_and_ordered() {
        let a = make("uuid7", "");
        let b = make("uuid7", "");
        assert!(is_canonical_uuid(&a, '7'), "{a}");
        assert!(a < b, "{a} then {b}");
    }

    #[test]
    fn uuidzero_is_nil() {
        assert_eq!(make("uuidzero", ""), "00000000-0000-0000-0000-000000000000");
    }

    #[test]
    fn ulid_and_nanoid_have_their_shapes() {
        let ulid = make("ulid", "");
        assert_eq!(ulid.len(), 26, "{ulid}");
        assert!(ulid.chars().all(|c| c.is_ascii_digit() || c.is_ascii_uppercase()), "{ulid}");
        let nano = make("nanoid", "");
        assert_eq!(nano.len(), 21, "{nano}");
        assert!(nano.bytes().all(|b| NANOID_ALPHABET.contains(&b)), "{nano}");
        assert_ne!(nano, make("nanoid", ""));
    }

    #[test]
    fn epoch_date_and_time_read_the_clock() {
        let epoch: i64 = make("epoch", "").parse().unwrap();
        assert!(epoch > 1_700_000_000, "{epoch}");
        let date = make("date", "");
        assert_eq!(date.len(), 10, "{date}");
        assert_eq!(date.as_bytes()[4], b'-');
        let time = make("time", "");
        assert_eq!(time.len(), 8, "{time}");
        assert_eq!(time.as_bytes()[2], b':');
        assert_eq!(make("date", "%Y").len(), 4);
        assert!(
            Generator::parse("time", "%")
                .unwrap()
                .unwrap()
                .generate()
                .unwrap_err()
                .starts_with("bad format `%`")
        );
    }

    #[test]
    fn lorem_counts_paragraphs_and_wants_a_number() {
        assert!(make("lorem", "").starts_with("Lorem ipsum"));
        assert!(!make("lorem", "").contains('\n'));
        let three = make("lorem", "3");
        assert_eq!(three.matches("\n\n").count(), 2);
        assert_ne!(three.lines().next(), three.lines().last(), "paragraphs differ");
        assert_eq!(
            Generator::parse("lorem", "many").unwrap().unwrap_err(),
            "lorem how many? `:lorem 3`"
        );
        assert_eq!(
            Generator::parse("lorem", "0").unwrap().unwrap_err(),
            "lorem how many? `:lorem 3`"
        );
    }

    #[test]
    fn password_has_the_length_asked_for() {
        assert_eq!(make("password", "").len(), PASSWORD_LEN);
        let p = make("password", "8");
        assert_eq!(p.len(), 8, "{p}");
        assert!(p.bytes().all(|b| PASSWORD_ALPHABET.contains(&b)), "{p}");
        assert_ne!(make("password", "40"), make("password", "40"));
    }

    #[test]
    fn a_bare_generator_refuses_an_argument() {
        assert_eq!(Generator::parse("uuid4", "x").unwrap().unwrap_err(), "uuid4 takes no argument");
    }
}
