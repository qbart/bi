//! Text that is known before it is typed: `:uuid4`, `:uuid7`, `:uuidzero`.
//!
//! Nothing in, text out. One enum rather than a command per kind, so that
//! the next generated thing — a timestamp, a lorem paragraph — is an arm
//! here and nothing new in the editor.
//!
//! See `docs/specs/generate.md`.

/// What to make.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Generator {
    /// `:uuid4` — random.
    Uuid4,
    /// `:uuid7` — time-ordered; two made in order sort in order.
    Uuid7,
    /// `:uuidzero` — the nil uuid.
    UuidZero,
}

impl Generator {
    /// The command's name, for messages.
    pub fn name(self) -> &'static str {
        match self {
            Generator::Uuid4 => "uuid4",
            Generator::Uuid7 => "uuid7",
            Generator::UuidZero => "uuidzero",
        }
    }

    /// A fresh value. Called once per cursor, which is what makes three
    /// cursors three different ids.
    pub fn generate(self) -> String {
        match self {
            Generator::Uuid4 => uuid::Uuid::new_v4(),
            Generator::Uuid7 => uuid::Uuid::now_v7(),
            Generator::UuidZero => uuid::Uuid::nil(),
        }
        .hyphenated()
        .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_canonical(id: &str, version: char) -> bool {
        id.len() == 36
            && id.chars().all(|c| c == '-' || c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            && id.chars().nth(14) == Some(version)
    }

    #[test]
    fn uuid4_is_canonical_and_random() {
        let (a, b) = (Generator::Uuid4.generate(), Generator::Uuid4.generate());
        assert!(is_canonical(&a, '4'), "{a}");
        assert_ne!(a, b);
    }

    #[test]
    fn uuid7_is_canonical_and_ordered() {
        let a = Generator::Uuid7.generate();
        let b = Generator::Uuid7.generate();
        assert!(is_canonical(&a, '7'), "{a}");
        assert!(a < b, "{a} then {b}");
    }

    #[test]
    fn uuidzero_is_nil() {
        assert_eq!(Generator::UuidZero.generate(), "00000000-0000-0000-0000-000000000000");
    }
}
