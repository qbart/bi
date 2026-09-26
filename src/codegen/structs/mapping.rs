//! The `.bimapping`: TOML that says which schema types the project
//! already has and how the generated code is spelled. Never what the
//! data means. See `docs/specs/gen-struct.md` §The mapping file.

use std::collections::BTreeMap;

use toml_edit::{Document, InlineTable, Item, Table, Value};

use super::Lang;
use crate::props::Diagnostic;

/// A type the project supplies: the spelling used where a field has it,
/// what to import for it, and what to write for a default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct External {
    pub spelling: String,
    pub import: Option<String>,
    pub default: Option<String>,
}

/// Templates for the three generics: `{T}` is the element's spelling,
/// `{S}` the ref target's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Generics {
    pub list: Option<String>,
    pub optional: Option<String>,
    pub reference: Option<String>,
}

/// One language's section, with the shared `[names]` folded in first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LangMapping {
    pub imports: Vec<String>,
    pub types: BTreeMap<String, External>,
    pub generics: Generics,
    pub names: Vec<(String, String)>,
    pub header: Option<String>,
    pub prefix: Option<String>,
    pub derive: Option<Vec<String>>,
}

impl LangMapping {
    /// The code name `[names]` gives `key` (`Type`, `Type.field`,
    /// `Enum.value`), the last one wins.
    pub fn renamed(&self, key: &str) -> Option<&str> {
        self.names.iter().rev().find(|(k, _)| k.as_str() == key).map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapping {
    pub ids: bool,
    /// Whether the data files' instances become constants, one file per
    /// data file.
    pub instances: bool,
    pub names: Vec<(String, String)>,
    pub langs: BTreeMap<Lang, LangMapping>,
    /// Whether this file said `ids` / `instances` at all — a later file
    /// that did not must not undo an earlier one that did.
    ids_set: bool,
    instances_set: bool,
}

impl Default for Mapping {
    fn default() -> Self {
        Mapping {
            ids: true,
            instances: true,
            names: Vec::new(),
            langs: BTreeMap::new(),
            ids_set: false,
            instances_set: false,
        }
    }
}

const LANG_KEYS: [&str; 7] =
    ["imports", "types", "generics", "names", "header", "prefix", "derive"];

impl Mapping {
    /// The file's text as a mapping, or every error in it, each
    /// `line N: …`.
    pub fn parse(text: &str) -> Result<Mapping, Vec<Diagnostic>> {
        let doc: Document<&str> = Document::parse(text).map_err(|e| {
            let line = e.span().map_or(1, |s| line_of(text, s.start));
            vec![Diagnostic::error(None, format!("line {line}: {}", e.message()))]
        })?;
        let mut out = Mapping::default();
        let mut errors = Vec::new();
        for (key, item) in doc.iter() {
            let line = line_for(&doc, key, text);
            match key {
                "ids" => match item.as_bool() {
                    Some(b) => {
                        out.ids = b;
                        out.ids_set = true;
                    }
                    None => errors.push(err(line, "ids must be true or false")),
                },
                "instances" => match item.as_bool() {
                    Some(b) => {
                        out.instances = b;
                        out.instances_set = true;
                    }
                    None => errors.push(err(line, "instances must be true or false")),
                },
                "names" => match entries(item, text) {
                    Some(e) => out.names.extend(read_names(e, "names", &mut errors)),
                    None => errors.push(err(line, "names must be a table")),
                },
                other => match Lang::parse(other).filter(|l| l.name() == other) {
                    Some(lang) => match entries(item, text) {
                        Some(e) => {
                            let m = read_lang(lang, e, text, &mut errors);
                            out.langs.insert(lang, m);
                        }
                        None => errors.push(err(line, format!("{other} must be a table"))),
                    },
                    None => errors.push(err(
                        line,
                        format!(
                            "unknown language `{other}` — {}",
                            Lang::ALL.map(Lang::name).join(", ")
                        ),
                    )),
                },
            }
        }
        if errors.is_empty() { Ok(out) } else { Err(errors) }
    }

    /// `later` wins on every key it sets; what it does not set stays.
    pub fn merge(&mut self, later: Mapping) {
        if later.ids_set {
            self.ids = later.ids;
            self.ids_set = true;
        }
        if later.instances_set {
            self.instances = later.instances;
            self.instances_set = true;
        }
        self.names.extend(later.names);
        for (lang, theirs) in later.langs {
            let mine = self.langs.entry(lang).or_default();
            if !theirs.imports.is_empty() {
                mine.imports = theirs.imports;
            }
            mine.types.extend(theirs.types);
            if theirs.generics.list.is_some() {
                mine.generics.list = theirs.generics.list;
            }
            if theirs.generics.optional.is_some() {
                mine.generics.optional = theirs.generics.optional;
            }
            if theirs.generics.reference.is_some() {
                mine.generics.reference = theirs.generics.reference;
            }
            mine.names.extend(theirs.names);
            if theirs.header.is_some() {
                mine.header = theirs.header;
            }
            if theirs.prefix.is_some() {
                mine.prefix = theirs.prefix;
            }
            if theirs.derive.is_some() {
                mine.derive = theirs.derive;
            }
        }
    }

    /// One language's section with the shared names in front, so a
    /// language's own rename wins over the shared one.
    pub fn for_lang(&self, lang: Lang) -> LangMapping {
        let mut m = self.langs.get(&lang).cloned().unwrap_or_default();
        let mut names = self.names.clone();
        names.append(&mut m.names);
        m.names = names;
        m
    }
}

fn err(line: usize, message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(None, format!("line {line}: {}", message.into()))
}

fn line_of(src: &str, offset: usize) -> usize {
    src[..offset.min(src.len())].matches('\n').count() + 1
}

fn line_for(table: &Table, key: &str, src: &str) -> usize {
    table.get_key_value(key).and_then(|(k, _)| k.span()).map_or(1, |s| line_of(src, s.start))
}

/// A table however TOML spelled it — `[a.b]` or `a.b = { … }` — as one
/// list of `(key, line, value)`.
fn entries<'a>(item: &'a Item, src: &str) -> Option<Vec<(String, usize, Entry<'a>)>> {
    match item {
        Item::Table(t) => Some(
            t.iter().map(|(k, v)| (k.to_string(), line_for(t, k, src), Entry::Item(v))).collect(),
        ),
        Item::Value(Value::InlineTable(t)) => Some(
            t.iter()
                .map(|(k, v)| (k.to_string(), line_in(t, k, src), Entry::Value(v.clone())))
                .collect(),
        ),
        _ => None,
    }
}

fn line_in(table: &InlineTable, key: &str, src: &str) -> usize {
    table.get_key_value(key).and_then(|(k, _)| k.span()).map_or(1, |s| line_of(src, s.start))
}

/// One entry of a table: an item from a `[section]`, or a value from an
/// inline table. The readers below only need scalars, arrays and tables
/// out of either.
enum Entry<'a> {
    Item(&'a Item),
    Value(Value),
}

impl Entry<'_> {
    fn as_str(&self) -> Option<&str> {
        match self {
            Entry::Item(i) => i.as_str(),
            Entry::Value(v) => v.as_str(),
        }
    }

    fn as_array(&self) -> Option<Vec<Value>> {
        let arr = match self {
            Entry::Item(i) => i.as_array()?,
            Entry::Value(v) => v.as_array()?,
        };
        Some(arr.iter().cloned().collect())
    }

    fn as_item(&self) -> Item {
        match self {
            Entry::Item(i) => (*i).clone(),
            Entry::Value(v) => Item::Value(v.clone()),
        }
    }
}

fn read_names(
    items: Vec<(String, usize, Entry)>,
    section: &str,
    errors: &mut Vec<Diagnostic>,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (key, line, value) in items {
        match value.as_str() {
            Some(s) => out.push((key, s.to_string())),
            None => errors.push(err(line, format!("{section}.{key} must be a string"))),
        }
    }
    out
}

fn read_lang(
    lang: Lang,
    items: Vec<(String, usize, Entry)>,
    src: &str,
    errors: &mut Vec<Diagnostic>,
) -> LangMapping {
    let mut m = LangMapping::default();
    let section = lang.name();
    for (key, line, value) in items {
        match key.as_str() {
            "imports" => {
                m.imports = read_strings(&value, line, &format!("{section}.imports"), errors)
            }
            "derive" => {
                m.derive = Some(read_strings(&value, line, &format!("{section}.derive"), errors))
            }
            "header" | "prefix" => match value.as_str() {
                Some(s) if key == "header" => m.header = Some(s.to_string()),
                Some(s) => m.prefix = Some(s.to_string()),
                None => errors.push(err(line, format!("{section}.{key} must be a string"))),
            },
            "names" => match entries(&value.as_item(), src) {
                Some(e) => m.names = read_names(e, &format!("{section}.names"), errors),
                None => errors.push(err(line, format!("{section}.names must be a table"))),
            },
            "generics" => match entries(&value.as_item(), src) {
                Some(e) => read_generics(e, section, &mut m.generics, errors),
                None => errors.push(err(line, format!("{section}.generics must be a table"))),
            },
            "types" => match entries(&value.as_item(), src) {
                Some(e) => m.types = read_types(e, section, src, errors),
                None => errors.push(err(line, format!("{section}.types must be a table"))),
            },
            other => errors.push(err(
                line,
                format!("unknown key `{other}` under [{section}] — {}", LANG_KEYS.join(", ")),
            )),
        }
    }
    m
}

fn read_strings(
    value: &Entry,
    line: usize,
    what: &str,
    errors: &mut Vec<Diagnostic>,
) -> Vec<String> {
    let Some(items) = value.as_array() else {
        errors.push(err(line, format!("{what} must be an array of strings")));
        return Vec::new();
    };
    let mut out = Vec::new();
    for v in items {
        match v.as_str() {
            Some(s) => out.push(s.to_string()),
            None => errors.push(err(line, format!("{what} must be an array of strings"))),
        }
    }
    out
}

fn read_generics(
    items: Vec<(String, usize, Entry)>,
    section: &str,
    generics: &mut Generics,
    errors: &mut Vec<Diagnostic>,
) {
    for (key, line, value) in items {
        let Some(s) = value.as_str() else {
            errors.push(err(line, format!("{section}.generics.{key} must be a string")));
            continue;
        };
        let (slot, hole) = match key.as_str() {
            "list" => (&mut generics.list, "{T}"),
            "optional" => (&mut generics.optional, "{T}"),
            "ref" => (&mut generics.reference, "{S}"),
            other => {
                errors.push(err(
                    line,
                    format!(
                        "unknown key `{other}` under [{section}.generics] — list, optional, ref"
                    ),
                ));
                continue;
            }
        };
        if !s.contains(hole) {
            errors.push(err(line, format!("{section}.generics.{key} must contain {hole}")));
            continue;
        }
        *slot = Some(s.to_string());
    }
}

fn read_types(
    items: Vec<(String, usize, Entry)>,
    section: &str,
    src: &str,
    errors: &mut Vec<Diagnostic>,
) -> BTreeMap<String, External> {
    let mut out = BTreeMap::new();
    for (key, line, value) in items {
        if let Some(s) = value.as_str() {
            out.insert(key, External { spelling: s.to_string(), ..Default::default() });
            continue;
        }
        let item = value.as_item();
        let Some(fields) = entries(&item, src) else {
            errors.push(err(line, format!("{section}.types.{key} must be a string or {{ as = …, import = …, default = … }}")));
            continue;
        };
        let mut ext = External::default();
        let mut has_as = false;
        for (fkey, fline, fvalue) in fields {
            let Some(s) = fvalue.as_str() else {
                errors.push(err(fline, format!("{section}.types.{key}.{fkey} must be a string")));
                continue;
            };
            match fkey.as_str() {
                "as" => {
                    has_as = true;
                    ext.spelling = s.to_string();
                }
                "import" => ext.import = Some(s.to_string()),
                "default" => ext.default = Some(s.to_string()),
                other => errors.push(err(
                    fline,
                    format!(
                        "unknown key `{other}` under {section}.types.{key} — as, import, default"
                    ),
                )),
            }
        }
        if !has_as {
            errors.push(err(line, format!("{section}.types.{key} needs `as`")));
            continue;
        }
        out.insert(key, ext);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r##"
ids = false
[names]
"Weapon.type" = "kind"
[rust]
imports = ["use serde::Deserialize;"]
derive = ["Debug"]
header = "#![allow(dead_code)]"
[rust.types]
Vec2 = "glam::Vec2"
rgb = { as = "Srgb<u8>", import = "use palette::Srgb;", default = "Srgb::new(0, 0, 0)" }
[rust.generics]
list = "SmallVec<[{T}; 4]>"
ref = "Handle<{S}>"
[rust.names]
"Weapon.name" = "display_name"
[cpp]
imports = ["#include <glm/vec2.hpp>"]
[c]
prefix = "gs_"
"##;

    #[test]
    fn every_key_parses() {
        let m = Mapping::parse(FULL).unwrap();
        assert!(!m.ids);
        assert_eq!(m.names, vec![("Weapon.type".to_string(), "kind".to_string())]);
        let r = m.for_lang(Lang::Rust);
        assert_eq!(r.imports, vec!["use serde::Deserialize;"]);
        assert_eq!(r.derive, Some(vec!["Debug".to_string()]));
        assert_eq!(r.header.as_deref(), Some("#![allow(dead_code)]"));
        assert_eq!(
            r.types["Vec2"],
            External { spelling: "glam::Vec2".into(), import: None, default: None }
        );
        assert_eq!(r.types["rgb"].import.as_deref(), Some("use palette::Srgb;"));
        assert_eq!(r.types["rgb"].default.as_deref(), Some("Srgb::new(0, 0, 0)"));
        assert_eq!(r.generics.list.as_deref(), Some("SmallVec<[{T}; 4]>"));
        assert_eq!(r.generics.reference.as_deref(), Some("Handle<{S}>"));
        assert_eq!(
            r.names,
            vec![
                ("Weapon.type".to_string(), "kind".to_string()),
                ("Weapon.name".to_string(), "display_name".to_string())
            ]
        );
        assert_eq!(r.renamed("Weapon.name"), Some("display_name"));
        assert_eq!(m.for_lang(Lang::C).prefix.as_deref(), Some("gs_"));
        assert_eq!(m.for_lang(Lang::Cpp).imports, vec!["#include <glm/vec2.hpp>"]);
        assert_eq!(
            m.for_lang(Lang::Go),
            LangMapping {
                names: vec![("Weapon.type".to_string(), "kind".to_string())],
                ..Default::default()
            },
            "a language with no section still gets the shared names"
        );
    }

    #[test]
    fn inline_tables_read_the_same() {
        let m = Mapping::parse(
            "rust = { types = { Vec2 = \"V\" }, generics = { list = \"L<{T}>\" } }\n",
        )
        .unwrap();
        let r = m.for_lang(Lang::Rust);
        assert_eq!(r.types["Vec2"].spelling, "V");
        assert_eq!(r.generics.list.as_deref(), Some("L<{T}>"));
    }

    #[test]
    fn unknown_keys_and_bad_templates_are_refused_with_a_line() {
        let e = Mapping::parse("[rust]\nderives = [\"Debug\"]\n").unwrap_err();
        assert_eq!(
            e[0].message,
            "line 2: unknown key `derives` under [rust] — imports, types, generics, names, header, prefix, derive"
        );
        let e = Mapping::parse("[rust.generics]\nlist = \"Vec\"\n").unwrap_err();
        assert_eq!(e[0].message, "line 2: rust.generics.list must contain {T}");
        let e = Mapping::parse("[rust.generics]\nref = \"Vec<{T}>\"\n").unwrap_err();
        assert_eq!(e[0].message, "line 2: rust.generics.ref must contain {S}");
        let e = Mapping::parse("[java]\n").unwrap_err();
        assert_eq!(e[0].message, "line 1: unknown language `java` — c, cpp, go, rust, c3, lua");
        let e = Mapping::parse("ids = 3\n").unwrap_err();
        assert_eq!(e[0].message, "line 1: ids must be true or false");
        assert!(Mapping::parse("[rust\n").unwrap_err()[0].message.starts_with("line 1: "));
        let e = Mapping::parse("[rust.types]\nVec2 = { import = \"x\" }\n").unwrap_err();
        assert_eq!(e[0].message, "line 2: rust.types.Vec2 needs `as`");
        let e = Mapping::parse("\n[c]\nimports = \"x\"\n").unwrap_err();
        assert_eq!(e[0].message, "line 3: c.imports must be an array of strings");
    }

    #[test]
    fn later_wins_per_key() {
        let mut a =
            Mapping::parse("ids = false\n[rust.types]\nVec2 = \"a::Vec2\"\nVec3 = \"a::Vec3\"\n")
                .unwrap();
        let b =
            Mapping::parse("[rust.types]\nVec2 = \"b::Vec2\"\n[rust]\nprefix = \"x\"\n").unwrap();
        a.merge(b);
        let r = a.for_lang(Lang::Rust);
        assert!(!a.ids, "a later file that says nothing about ids leaves them");
        assert_eq!(r.types["Vec2"].spelling, "b::Vec2");
        assert_eq!(r.types["Vec3"].spelling, "a::Vec3");
        assert_eq!(r.prefix.as_deref(), Some("x"));
        let mut c = Mapping::default();
        c.merge(Mapping::parse("ids = false\n").unwrap());
        assert!(!c.ids);
    }

    #[test]
    fn the_empty_mapping_is_the_default() {
        let m = Mapping::parse("").unwrap();
        assert!(m.ids);
        assert!(m.instances);
        assert!(!Mapping::parse("instances = false\n").unwrap().instances);
        assert_eq!(
            Mapping::parse("instances = 1\n").unwrap_err()[0].message,
            "line 1: instances must be true or false"
        );
        assert!(m.langs.is_empty());
        assert_eq!(m, Mapping::default());
    }
}
