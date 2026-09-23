//! The bi property format: schemas of struct-like types, data files of
//! typed instances, and the property view that edits both. No editor in
//! here — the view is plain state a frontend draws, and every edit is a
//! new text for the buffer to hold. See `docs/specs/bi-format.md` for the
//! format and `docs/specs/props.md` for the view.

pub mod data;
pub mod schema;
pub mod view;

use std::path::Path;

use serde_json::Value;

pub use view::{Edit, Props, Refactor, Row, RowKind, RowWidget};

/// The format's major version, as `$dialect` spells it.
pub const DIALECT: &str = "bi/1";
pub const MAJOR: u64 = 1;

/// Which of the two file kinds a file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Schema,
    Data,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Schema => "bischema",
            Kind::Data => "bidata",
        }
    }

    pub fn parse(name: &str) -> Option<Kind> {
        match name {
            "bischema" => Some(Kind::Schema),
            "bidata" => Some(Kind::Data),
            _ => None,
        }
    }
}

/// The kind a path's extension names, if it names one.
pub fn kind_of(path: &Path) -> Option<Kind> {
    let ext = path.extension()?.to_str()?;
    Kind::parse(&ext.to_ascii_lowercase())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Error,
    Warning,
}

/// One thing wrong with a file. `at` is a row key of the view (see
/// `view.rs`), so a warning lands on the row it is about; `None` is the
/// file as a whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub level: Level,
    pub at: Option<String>,
    pub message: String,
}

impl Diagnostic {
    pub fn error(at: Option<&str>, message: impl Into<String>) -> Self {
        Self { level: Level::Error, at: at.map(str::to_string), message: message.into() }
    }

    pub fn warning(at: Option<&str>, message: impl Into<String>) -> Self {
        Self { level: Level::Warning, at: at.map(str::to_string), message: message.into() }
    }
}

/// What `$dialect` said, when it did not say `bi/1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dialect {
    Missing,
    Unknown(String),
    Older(u64),
    Newer(u64),
}

impl Dialect {
    pub fn message(&self) -> String {
        match self {
            Dialect::Missing => "no $dialect (want \"bi/1\")".into(),
            Dialect::Unknown(s) => format!("unknown $dialect {s:?} (want \"bi/1\")"),
            Dialect::Older(n) => format!("$dialect bi/{n} is older than bi/{MAJOR} (:bi migrate)"),
            Dialect::Newer(n) => format!("$dialect bi/{n} is newer than bi/{MAJOR}"),
        }
    }
}

/// The header check every file goes through first.
pub fn check_dialect(doc: &Value) -> Result<(), Dialect> {
    let Some(dialect) = doc.get("$dialect") else { return Err(Dialect::Missing) };
    let Some(text) = dialect.as_str() else {
        return Err(Dialect::Unknown(dialect.to_string()));
    };
    let Some(version) = text.strip_prefix("bi/") else {
        return Err(Dialect::Unknown(text.into()));
    };
    let Ok(major) = version.parse::<u64>() else { return Err(Dialect::Unknown(text.into())) };
    match major.cmp(&MAJOR) {
        std::cmp::Ordering::Less => Err(Dialect::Older(major)),
        std::cmp::Ordering::Greater => Err(Dialect::Newer(major)),
        std::cmp::Ordering::Equal => Ok(()),
    }
}

/// `[A-Za-z_][A-Za-z0-9_]*`.
pub fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// The first error's message, with a count when there are more.
pub fn summary(diagnostics: &[Diagnostic]) -> Option<String> {
    let errors: Vec<&Diagnostic> = diagnostics.iter().filter(|d| d.level == Level::Error).collect();
    let first = errors.first()?;
    Some(match errors.len() {
        1 => first.message.clone(),
        n => format!("{} (+{} more)", first.message, n - 1),
    })
}

/// JSON equality the format means: `12` and `12.0` are the same number.
pub fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_i64(), y.as_i64()) {
            (Some(x), Some(y)) => x == y,
            _ => x.as_f64() == y.as_f64(),
        },
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| json_eq(a, b))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| json_eq(v, w)))
        }
        _ => a == b,
    }
}

// ---- the writer ----------------------------------------------------------

/// A schema's canonical text: one property per line at the top, in
/// `types`, in each definition and in `fields`; everything deeper inline.
pub fn write_schema(doc: &Value) -> String {
    write(doc, &|path| match path {
        [] | ["types"] => true,
        ["types", _] => true,
        ["types", _, "fields"] => true,
        _ => false,
    })
}

/// A data file's canonical text: one property per line at the top, in
/// `instances` and in each instance; everything deeper inline.
pub fn write_data(doc: &Value) -> String {
    write(doc, &|path| matches!(path, [] | ["instances"] | ["instances", _]))
}

/// The layout a `Kind` writes.
pub fn write_kind(kind: Kind, doc: &Value) -> String {
    match kind {
        Kind::Schema => write_schema(doc),
        Kind::Data => write_data(doc),
    }
}

/// A value written with the containers `expand` names one entry per line
/// and every other container inline. Array indexes appear in the path as
/// `#`. Ends with a newline.
fn write(doc: &Value, expand: &dyn Fn(&[&str]) -> bool) -> String {
    let mut out = String::new();
    let mut path = Vec::new();
    write_value(doc, &mut path, 0, expand, &mut out);
    out.push('\n');
    out
}

fn write_value<'a>(
    value: &'a Value,
    path: &mut Vec<&'a str>,
    indent: usize,
    expand: &dyn Fn(&[&str]) -> bool,
    out: &mut String,
) {
    match value {
        Value::Object(map) if map.is_empty() => out.push_str("{}"),
        Value::Array(items) if items.is_empty() => out.push_str("[]"),
        Value::Object(map) if expand(path) => {
            out.push_str("{\n");
            let pad = "  ".repeat(indent + 1);
            for (i, (key, item)) in map.iter().enumerate() {
                out.push_str(&pad);
                out.push_str(&serde_json::to_string(key).unwrap_or_default());
                out.push_str(": ");
                path.push(key);
                write_value(item, path, indent + 1, expand, out);
                path.pop();
                out.push_str(if i + 1 < map.len() { ",\n" } else { "\n" });
            }
            out.push_str(&"  ".repeat(indent));
            out.push('}');
        }
        Value::Array(items) if expand(path) => {
            out.push_str("[\n");
            let pad = "  ".repeat(indent + 1);
            for (i, item) in items.iter().enumerate() {
                out.push_str(&pad);
                path.push("#");
                write_value(item, path, indent + 1, expand, out);
                path.pop();
                out.push_str(if i + 1 < items.len() { ",\n" } else { "\n" });
            }
            out.push_str(&"  ".repeat(indent));
            out.push(']');
        }
        Value::Object(map) => {
            out.push_str("{ ");
            for (i, (key, item)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&serde_json::to_string(key).unwrap_or_default());
                out.push_str(": ");
                write_inline(item, out);
            }
            out.push_str(" }");
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_inline(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

/// Inside an inline container everything is inline, whatever `expand`
/// would have said one level up.
fn write_inline(value: &Value, out: &mut String) {
    let mut path = Vec::new();
    write_value(value, &mut path, 0, &|_| false, out);
}

#[cfg(test)]
pub(crate) mod fixtures {
    pub const SCHEMA: &str = r#"{
  "$dialect": "bi/1",
  "types": {
    "Rarity": {
      "kind": "enum",
      "values": ["common", "rare", "epic"],
      "doc": "Drop tier, drives colour and loot tables"
    },
    "Vec2": {
      "kind": "struct",
      "fields": [
        { "name": "x", "type": "f32", "default": 0 },
        { "name": "y", "type": "f32", "default": 0 }
      ]
    },
    "Weapon": {
      "kind": "struct",
      "fields": [
        { "name": "name", "type": "string" },
        { "name": "damage", "type": "i32", "default": 10, "min": 0, "max": 999 },
        { "name": "rarity", "type": "Rarity", "default": "common" },
        { "name": "offset", "type": "Vec2", "default": { "x": 0.5 } },
        { "name": "tags", "type": "list<string>" },
        { "name": "two_handed", "type": "bool", "default": false }
      ]
    },
    "Enemy": {
      "kind": "struct",
      "fields": [
        { "name": "name", "type": "string" },
        { "name": "hp", "type": "i32", "default": 100, "min": 1 },
        { "name": "speed", "type": "f32", "default": 1.0, "min": 0, "max": 10, "step": 0.1 },
        { "name": "weapon", "type": "ref<Weapon>" },
        { "name": "drops", "type": "list<ref<Weapon>>" },
        { "name": "spawn", "type": "Vec2" },
        { "name": "leader", "type": "optional<ref<Enemy>>" }
      ]
    }
  }
}
"#;

    pub const DATA: &str = r#"{
  "$dialect": "bi/1",
  "$schema": "game.bischema",
  "instances": [
    {
      "$type": "Weapon",
      "$id": "rusty_sword",
      "name": "Rusty Sword",
      "damage": 12,
      "tags": ["melee", "starter"]
    },
    {
      "$type": "Weapon",
      "$id": "dagger",
      "name": "Dagger",
      "damage": 7,
      "rarity": "rare",
      "offset": { "x": 0.25, "y": -0.1 },
      "tags": ["melee"]
    },
    {
      "$type": "Weapon",
      "$id": "warhammer",
      "name": "Warhammer",
      "damage": 40,
      "rarity": "epic",
      "two_handed": true
    },
    {
      "$type": "Enemy",
      "$id": "goblin",
      "name": "Goblin",
      "hp": 40,
      "speed": 1.4,
      "weapon": "rusty_sword",
      "drops": ["rusty_sword", "dagger"],
      "spawn": { "x": 12, "y": 3 },
      "leader": "goblin_chief"
    },
    {
      "$type": "Enemy",
      "$id": "goblin_chief",
      "name": "Goblin Chief",
      "hp": 120,
      "weapon": "warhammer",
      "drops": ["warhammer"],
      "spawn": { "x": 20, "y": 3 }
    }
  ]
}
"#;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dialect_header_is_checked_by_major_version() {
        let ok: Value = serde_json::from_str(r#"{"$dialect":"bi/1"}"#).unwrap();
        assert_eq!(check_dialect(&ok), Ok(()));
        let missing: Value = serde_json::from_str(r#"{"types":{}}"#).unwrap();
        assert_eq!(check_dialect(&missing), Err(Dialect::Missing));
        let odd: Value = serde_json::from_str(r#"{"$dialect":"yaml/1"}"#).unwrap();
        assert_eq!(check_dialect(&odd), Err(Dialect::Unknown("yaml/1".into())));
        let old: Value = serde_json::from_str(r#"{"$dialect":"bi/0"}"#).unwrap();
        assert_eq!(check_dialect(&old), Err(Dialect::Older(0)));
        let new: Value = serde_json::from_str(r#"{"$dialect":"bi/2"}"#).unwrap();
        assert_eq!(check_dialect(&new), Err(Dialect::Newer(2)));
        assert_eq!(Dialect::Older(0).message(), "$dialect bi/0 is older than bi/1 (:bi migrate)");
    }

    #[test]
    fn the_kind_comes_from_the_extension() {
        assert_eq!(kind_of(Path::new("game.bischema")), Some(Kind::Schema));
        assert_eq!(kind_of(Path::new("a/level1.BIDATA")), Some(Kind::Data));
        assert_eq!(kind_of(Path::new("level1.json")), None);
        assert_eq!(kind_of(Path::new("bidata")), None);
    }

    #[test]
    fn identifiers_and_numeric_equality() {
        assert!(is_identifier("rusty_sword"));
        assert!(is_identifier("_1"));
        assert!(!is_identifier("1a"));
        assert!(!is_identifier("$id"));
        assert!(!is_identifier(""));
        assert!(!is_identifier("a-b"));
        let (a, b): (Value, Value) = (serde_json::json!(12), serde_json::json!(12.0));
        assert!(json_eq(&a, &b));
        assert!(!json_eq(&serde_json::json!(12), &serde_json::json!(12.5)));
        assert!(json_eq(
            &serde_json::json!({"x": 1, "y": [1.0]}),
            &serde_json::json!({"x": 1.0, "y": [1]})
        ));
    }

    #[test]
    fn the_writer_round_trips_the_examples_byte_for_byte() {
        let schema: Value = serde_json::from_str(fixtures::SCHEMA).unwrap();
        assert_eq!(write_schema(&schema), fixtures::SCHEMA);
        let data: Value = serde_json::from_str(fixtures::DATA).unwrap();
        assert_eq!(write_data(&data), fixtures::DATA);
    }

    #[test]
    fn empty_containers_and_nesting_write_compactly() {
        let doc: Value = serde_json::from_str(
            r#"{"$dialect":"bi/1","$schema":"s","instances":[{"$type":"A","$id":"a","l":[],"o":{},"n":[[1,2],{"k":"v"}]}]}"#,
        )
        .unwrap();
        assert_eq!(
            write_data(&doc),
            "{\n  \"$dialect\": \"bi/1\",\n  \"$schema\": \"s\",\n  \"instances\": [\n    {\n      \"$type\": \"A\",\n      \"$id\": \"a\",\n      \"l\": [],\n      \"o\": {},\n      \"n\": [[1, 2], { \"k\": \"v\" }]\n    }\n  ]\n}\n"
        );
        let empty: Value = serde_json::from_str(r#"{"$dialect":"bi/1","types":{}}"#).unwrap();
        assert_eq!(write_schema(&empty), "{\n  \"$dialect\": \"bi/1\",\n  \"types\": {}\n}\n");
    }
}
