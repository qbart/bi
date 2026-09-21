//! A `.bidata`: instances of one schema's structs, stored sparsely, and
//! the index over every data file of the schema that refs resolve
//! through. See `docs/specs/bi-format.md`.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::schema::{Schema, TypeDef, TypeExpr};
use super::{Diagnostic, check_dialect, is_identifier, json_eq};

#[derive(Debug, Clone, PartialEq)]
pub struct Instance {
    pub ty: String,
    pub id: String,
    /// Every key that is not `$`-prefixed, as stored.
    pub values: Map<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DataFile {
    /// `$schema` as written: relative to the data file, forward slashes.
    pub schema: String,
    pub instances: Vec<Instance>,
}

/// The file's text read into its raw value and the instances it holds,
/// or every error found. Values are not checked here — that needs the
/// schema, which is `validate`'s.
pub fn parse(text: &str) -> Result<(Value, DataFile), Vec<Diagnostic>> {
    let doc: Value = match serde_json::from_str(text) {
        Ok(doc) => doc,
        Err(e) => return Err(vec![Diagnostic::error(None, format!("invalid JSON: {e}"))]),
    };
    let data = from_value(&doc)?;
    Ok((doc, data))
}

pub fn from_value(doc: &Value) -> Result<DataFile, Vec<Diagnostic>> {
    let mut errors = Vec::new();
    if let Err(d) = check_dialect(doc) {
        errors.push(Diagnostic::error(None, d.message()));
    }
    let schema = match doc.get("$schema").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            errors.push(Diagnostic::error(None, "no $schema"));
            String::new()
        }
    };
    let Some(items) = doc.get("instances").and_then(Value::as_array) else {
        errors.push(Diagnostic::error(None, "no instances array"));
        return Err(errors);
    };
    let mut instances = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let at = format!("inst:{i}");
        let Some(map) = item.as_object() else {
            errors.push(Diagnostic::error(Some(&at), format!("instances[{i}] is not an object")));
            continue;
        };
        let ty = match map.get("$type").and_then(Value::as_str) {
            Some(t) => t.to_string(),
            None => {
                errors.push(Diagnostic::error(Some(&at), format!("instances[{i}] has no $type")));
                continue;
            }
        };
        let id = match map.get("$id").and_then(Value::as_str) {
            Some(id) if is_identifier(id) => id.to_string(),
            Some(id) => {
                errors.push(Diagnostic::error(
                    Some(&at),
                    format!("{ty} $id {id:?} is not an identifier"),
                ));
                continue;
            }
            None => {
                errors.push(Diagnostic::error(
                    Some(&at),
                    format!("instances[{i}] ({ty}) has no $id"),
                ));
                continue;
            }
        };
        let values = map
            .iter()
            .filter(|(k, _)| !k.starts_with('$'))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        instances.push(Instance { ty, id, values });
    }
    if errors.is_empty() { Ok(DataFile { schema, instances }) } else { Err(errors) }
}

/// Where every instance of the schema's *other* data files lives, so a
/// ref can be checked and followed across files. This file's own
/// instances are not in it — they are in hand.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Index {
    pub entries: Vec<(String, String, PathBuf)>,
}

impl Index {
    /// The index over `files`, each as its path and text; a file that
    /// does not parse contributes nothing.
    pub fn build(files: &[(PathBuf, String)]) -> Index {
        let mut entries = Vec::new();
        for (path, text) in files {
            if let Ok((_, data)) = parse(text) {
                for inst in data.instances {
                    entries.push((inst.ty, inst.id, path.clone()));
                }
            }
        }
        Index { entries }
    }

    pub fn has(&self, ty: &str, id: &str) -> bool {
        self.entries.iter().any(|(t, i, _)| t == ty && i == id)
    }

    pub fn file_of(&self, ty: &str, id: &str) -> Option<&Path> {
        self.entries.iter().find(|(t, i, _)| t == ty && i == id).map(|(_, _, p)| p.as_path())
    }

    pub fn ids_of<'a>(&'a self, ty: &'a str) -> impl Iterator<Item = &'a str> {
        self.entries.iter().filter(move |(t, _, _)| t == ty).map(|(_, i, _)| i.as_str())
    }

    pub fn files(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        for (_, _, p) in &self.entries {
            if !out.contains(p) {
                out.push(p.clone());
            }
        }
        out
    }
}

/// Every check of the format's data table below the header: types are
/// structs, ids unique here and across the index, values encode their
/// fields, refs resolve, required refs are set, unknown keys are named.
pub fn validate(data: &DataFile, schema: &Schema, index: &Index) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let known = |ty: &str, id: &str| {
        data.instances.iter().any(|i| i.ty == ty && i.id == id) || index.has(ty, id)
    };
    for (i, inst) in data.instances.iter().enumerate() {
        let at = format!("inst:{i}");
        let Some(fields) = schema.fields_of(&inst.ty) else {
            let what = if schema.get(&inst.ty).is_some() { "an enum" } else { "not in the schema" };
            out.push(Diagnostic::error(Some(&at), format!("$type {} is {what}", inst.ty)));
            continue;
        };
        let twice_here = data.instances[..i].iter().any(|o| o.ty == inst.ty && o.id == inst.id);
        if twice_here {
            out.push(Diagnostic::error(
                Some(&at),
                format!("{} {} twice in this file", inst.ty, inst.id),
            ));
        } else if let Some(file) = index.file_of(&inst.ty, &inst.id) {
            out.push(Diagnostic::error(
                Some(&at),
                format!("{} {} is also in {}", inst.ty, inst.id, name_of(file)),
            ));
        }
        for field in fields {
            let key = format!("{at}/{}", field.name);
            match inst.values.get(&field.name) {
                Some(v) => schema.check_field(field, v, &key, &mut out, Some(&known)),
                None => {
                    if matches!(field.ty, TypeExpr::Ref(_)) && field.default.is_none() {
                        out.push(Diagnostic::warning(
                            Some(&key),
                            format!("{} not set", field.ty.text()),
                        ));
                    }
                }
            }
        }
        for k in inst.values.keys() {
            if !fields.iter().any(|f| &f.name == k) {
                out.push(Diagnostic::warning(
                    Some(&format!("{at}/?{k}")),
                    format!("unknown key {k}"),
                ));
            }
        }
    }
    out
}

/// The document as `:w` writes it: keys equal to their default gone,
/// keys in the schema's order behind `$type` and `$id`, unknown keys
/// after in their own order.
pub fn normalise(doc: &Value, schema: &Schema) -> Value {
    let Some(items) = doc.get("instances").and_then(Value::as_array) else { return doc.clone() };
    let instances: Vec<Value> = items.iter().map(|item| normalise_instance(item, schema)).collect();
    let mut out = doc.clone();
    out["instances"] = Value::Array(instances);
    out
}

fn normalise_instance(item: &Value, schema: &Schema) -> Value {
    let Some(map) = item.as_object() else { return item.clone() };
    let ty = map.get("$type").and_then(Value::as_str).unwrap_or("");
    let Some(fields) = schema.fields_of(ty) else { return item.clone() };
    let mut out = Map::new();
    for key in ["$type", "$id"] {
        if let Some(v) = map.get(key) {
            out.insert(key.into(), v.clone());
        }
    }
    for (k, v) in map {
        if k.starts_with('$') && !out.contains_key(k) {
            out.insert(k.clone(), v.clone());
        }
    }
    for field in fields {
        if let Some(v) = map.get(&field.name)
            && !json_eq(&schema.resolve(&field.ty, v), &schema.field_default(field))
        {
            out.insert(field.name.clone(), schema.sparse(&field.ty, v));
        }
    }
    for (k, v) in map {
        if !out.contains_key(k) && !k.starts_with('$') && !fields.iter().any(|f| &f.name == k) {
            out.insert(k.clone(), v.clone());
        }
    }
    Value::Object(out)
}

/// The data files of `schema` under `root`: every `.bidata` whose
/// `$schema` resolves to it, what git ignores skipped, `except` left
/// out (the file in hand).
pub fn project_files(root: &Path, schema: &Path, except: Option<&Path>) -> Vec<PathBuf> {
    let Some(schema) = canonical(schema) else { return Vec::new() };
    let except = except.and_then(canonical);
    let walk = ignore::WalkBuilder::new(root).hidden(true).build();
    let mut out = Vec::new();
    for entry in walk.flatten() {
        let path = entry.path();
        if !entry.file_type().is_some_and(|t| t.is_file())
            || super::kind_of(path) != Some(super::Kind::Data)
        {
            continue;
        }
        let Some(here) = canonical(path) else { continue };
        if except.as_deref() == Some(here.as_path()) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else { continue };
        let Some(rel) = schema_of(&text) else { continue };
        if schema_path(path, &rel).and_then(|p| canonical(&p)).as_deref() == Some(schema.as_path())
        {
            out.push(path.to_path_buf());
        }
    }
    out.sort();
    out
}

/// `$schema` of a data file's text, read cheaply.
pub fn schema_of(text: &str) -> Option<String> {
    let doc: Value = serde_json::from_str(text).ok()?;
    doc.get("$schema")?.as_str().map(str::to_string)
}

/// Where a data file's `$schema` points: beside the data file.
pub fn schema_path(data: &Path, rel: &str) -> Option<PathBuf> {
    let dir = data.parent()?;
    Some(dir.join(rel))
}

fn canonical(path: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(path).ok()
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

// ---- the refactorings, one document at a time ----------------------------
//
// Each rewrites a raw data document in place and says whether anything
// changed; the editor runs them over this buffer and every other data
// file of the schema.

/// Every ref to `ty` `old` — and the instance's own `$id` — spelled `new`.
pub fn rename_id(doc: &mut Value, schema: &Schema, ty: &str, old: &str, new: &str) -> bool {
    let mut changed = false;
    let Some(items) = doc.get_mut("instances").and_then(Value::as_array_mut) else { return false };
    for item in items {
        let Some(map) = item.as_object_mut() else { continue };
        let this = map.get("$type").and_then(Value::as_str).unwrap_or("").to_string();
        if this == ty && map.get("$id").and_then(Value::as_str) == Some(old) {
            map.insert("$id".into(), Value::String(new.into()));
            changed = true;
        }
        let Some(fields) = schema.fields_of(&this) else { continue };
        for field in fields {
            if let Some(v) = map.get_mut(&field.name) {
                changed |= rename_ref_in(v, &field.ty, ty, old, new, schema);
            }
        }
    }
    changed
}

fn rename_ref_in(
    value: &mut Value,
    ty: &TypeExpr,
    target: &str,
    old: &str,
    new: &str,
    schema: &Schema,
) -> bool {
    match (ty, value) {
        (TypeExpr::Ref(t), Value::String(s)) if t == target && s == old => {
            *s = new.into();
            true
        }
        (TypeExpr::List(inner), Value::Array(items)) => {
            let mut changed = false;
            for v in items {
                changed |= rename_ref_in(v, inner, target, old, new, schema);
            }
            changed
        }
        (TypeExpr::Optional(inner), v) if !v.is_null() => {
            rename_ref_in(v, inner, target, old, new, schema)
        }
        (TypeExpr::Named(name), Value::Object(map)) => {
            let Some(fields) = schema.fields_of(name) else { return false };
            let mut changed = false;
            for field in fields {
                if let Some(v) = map.get_mut(&field.name) {
                    changed |= rename_ref_in(v, &field.ty, target, old, new, schema);
                }
            }
            changed
        }
        _ => false,
    }
}

/// The key `old` of every `ty` instance — and of every embedded `ty`
/// value — spelled `new`. `remap` is the same rewrite.
pub fn rename_field(doc: &mut Value, schema: &Schema, ty: &str, old: &str, new: &str) -> bool {
    let mut changed = false;
    let Some(items) = doc.get_mut("instances").and_then(Value::as_array_mut) else { return false };
    for item in items {
        let Some(map) = item.as_object_mut() else { continue };
        let this = map.get("$type").and_then(Value::as_str).unwrap_or("").to_string();
        if this == ty {
            changed |= rename_key(map, old, new);
        }
        let Some(fields) = schema.fields_of(&this) else { continue };
        for field in fields {
            if let Some(v) = map.get_mut(&field.name) {
                changed |= rename_field_in(v, &field.ty, ty, old, new, schema);
            }
        }
    }
    changed
}

fn rename_field_in(
    value: &mut Value,
    ty: &TypeExpr,
    target: &str,
    old: &str,
    new: &str,
    schema: &Schema,
) -> bool {
    match (ty, value) {
        (TypeExpr::Named(name), Value::Object(map)) => {
            let mut changed = false;
            if name == target {
                changed |= rename_key(map, old, new);
            }
            let Some(fields) = schema.fields_of(name) else { return changed };
            for field in fields {
                if let Some(v) = map.get_mut(&field.name) {
                    changed |= rename_field_in(v, &field.ty, target, old, new, schema);
                }
            }
            changed
        }
        (TypeExpr::List(inner), Value::Array(items)) => {
            let mut changed = false;
            for v in items {
                changed |= rename_field_in(v, inner, target, old, new, schema);
            }
            changed
        }
        (TypeExpr::Optional(inner), v) if !v.is_null() => {
            rename_field_in(v, inner, target, old, new, schema)
        }
        _ => false,
    }
}

/// A key renamed in place, keeping its position.
fn rename_key(map: &mut Map<String, Value>, old: &str, new: &str) -> bool {
    if !map.contains_key(old) || map.contains_key(new) {
        return false;
    }
    let entries: Vec<(String, Value)> = std::mem::take(map)
        .into_iter()
        .map(|(k, v)| if k == old { (new.to_string(), v) } else { (k, v) })
        .collect();
    map.extend(entries);
    true
}

/// Every stored `old` of enum `en` spelled `new`.
pub fn rename_enum_value(doc: &mut Value, schema: &Schema, en: &str, old: &str, new: &str) -> bool {
    let mut changed = false;
    let Some(items) = doc.get_mut("instances").and_then(Value::as_array_mut) else { return false };
    for item in items {
        let Some(map) = item.as_object_mut() else { continue };
        let this = map.get("$type").and_then(Value::as_str).unwrap_or("").to_string();
        let Some(fields) = schema.fields_of(&this) else { continue };
        for field in fields {
            if let Some(v) = map.get_mut(&field.name) {
                changed |= rename_enum_in(v, &field.ty, en, old, new, schema);
            }
        }
    }
    changed
}

fn rename_enum_in(
    value: &mut Value,
    ty: &TypeExpr,
    en: &str,
    old: &str,
    new: &str,
    schema: &Schema,
) -> bool {
    match (ty, value) {
        (TypeExpr::Named(name), Value::String(s)) if name == en && s == old => {
            *s = new.into();
            true
        }
        (TypeExpr::Named(name), Value::Object(map)) => {
            let Some(fields) = schema.fields_of(name) else { return false };
            let mut changed = false;
            for field in fields {
                if let Some(v) = map.get_mut(&field.name) {
                    changed |= rename_enum_in(v, &field.ty, en, old, new, schema);
                }
            }
            changed
        }
        (TypeExpr::List(inner), Value::Array(items)) => {
            let mut changed = false;
            for v in items {
                changed |= rename_enum_in(v, inner, en, old, new, schema);
            }
            changed
        }
        (TypeExpr::Optional(inner), v) if !v.is_null() => {
            rename_enum_in(v, inner, en, old, new, schema)
        }
        _ => false,
    }
}

/// Every key of every instance that its struct does not name, removed.
pub fn prune(doc: &mut Value, schema: &Schema) -> usize {
    let mut removed = 0;
    let Some(items) = doc.get_mut("instances").and_then(Value::as_array_mut) else { return 0 };
    for item in items {
        let Some(map) = item.as_object_mut() else { continue };
        let this = map.get("$type").and_then(Value::as_str).unwrap_or("").to_string();
        let Some(fields) = schema.fields_of(&this) else { continue };
        let unknown: Vec<String> = map
            .keys()
            .filter(|k| !k.starts_with('$') && !fields.iter().any(|f| &&f.name == k))
            .cloned()
            .collect();
        for k in unknown {
            map.shift_remove(&k);
            removed += 1;
        }
    }
    removed
}

/// Who refs `ty` `id` in this document: `(type, id)` of each instance.
pub fn referrers(data: &DataFile, schema: &Schema, ty: &str, id: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for inst in &data.instances {
        let Some(fields) = schema.fields_of(&inst.ty) else { continue };
        let hit = fields
            .iter()
            .any(|f| inst.values.get(&f.name).is_some_and(|v| refs_in(v, &f.ty, ty, id, schema)));
        if hit {
            out.push((inst.ty.clone(), inst.id.clone()));
        }
    }
    out
}

fn refs_in(value: &Value, ty: &TypeExpr, target: &str, id: &str, schema: &Schema) -> bool {
    match (ty, value) {
        (TypeExpr::Ref(t), Value::String(s)) => t == target && s == id,
        (TypeExpr::List(inner), Value::Array(items)) => {
            items.iter().any(|v| refs_in(v, inner, target, id, schema))
        }
        (TypeExpr::Optional(inner), v) if !v.is_null() => refs_in(v, inner, target, id, schema),
        (TypeExpr::Named(name), Value::Object(map)) => {
            schema.fields_of(name).is_some_and(|fields| {
                fields.iter().any(|f| {
                    map.get(&f.name).is_some_and(|v| refs_in(v, &f.ty, target, id, schema))
                })
            })
        }
        _ => false,
    }
}

/// Whether a schema type is named by any field of any other type — what
/// stops deleting one from under them.
pub fn type_used(schema: &Schema, name: &str) -> Option<String> {
    for (tname, def) in &schema.types {
        let TypeDef::Struct { fields, .. } = def else { continue };
        for f in fields {
            let mut names = Vec::new();
            names_in(&f.ty, &mut names);
            if names.iter().any(|n| n == name) {
                return Some(format!("{tname}.{}", f.name));
            }
        }
    }
    None
}

fn names_in(ty: &TypeExpr, out: &mut Vec<String>) {
    match ty {
        TypeExpr::Named(n) | TypeExpr::Ref(n) => out.push(n.clone()),
        TypeExpr::List(t) | TypeExpr::Optional(t) => names_in(t, out),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{DATA, SCHEMA};
    use super::super::{Level, write_data};
    use super::*;

    fn both() -> (Value, DataFile, Schema) {
        let (doc, data) = parse(DATA).unwrap();
        let (_, schema) = Schema::parse(SCHEMA).unwrap();
        (doc, data, schema)
    }

    #[test]
    fn the_example_is_clean() {
        let (_, data, schema) = both();
        assert_eq!(data.schema, "game.bischema");
        assert_eq!(data.instances.len(), 5);
        assert_eq!(validate(&data, &schema, &Index::default()), Vec::new());
    }

    fn messages(text: &str, index: &Index) -> Vec<(Level, String, String)> {
        let (_, schema) = Schema::parse(SCHEMA).unwrap();
        match parse(text) {
            Ok((_, data)) => validate(&data, &schema, index),
            Err(errors) => errors,
        }
        .into_iter()
        .map(|d| (d.level, d.at.unwrap_or_default(), d.message))
        .collect()
    }

    #[test]
    fn every_data_error_and_warning_is_named() {
        let file = |instances: &str| {
            format!(r#"{{"$dialect":"bi/1","$schema":"game.bischema","instances":{instances}}}"#)
        };
        let none = Index::default();
        assert_eq!(
            messages(r#"{"$dialect":"bi/1","instances":[]}"#, &none),
            [(Level::Error, "".into(), "no $schema".into())]
        );
        assert_eq!(
            messages(r#"{"$dialect":"bi/1","$schema":"x"}"#, &none),
            [(Level::Error, "".into(), "no instances array".into())]
        );
        assert_eq!(
            messages(&file(r#"[{"$type":"Rarity","$id":"a"}]"#), &none),
            [(Level::Error, "inst:0".into(), "$type Rarity is an enum".into())]
        );
        assert_eq!(
            messages(&file(r#"[{"$type":"Boss","$id":"a"}]"#), &none),
            [(Level::Error, "inst:0".into(), "$type Boss is not in the schema".into())]
        );
        assert_eq!(
            messages(&file(r#"[{"$type":"Vec2"}]"#), &none),
            [(Level::Error, "inst:0".into(), "instances[0] (Vec2) has no $id".into())]
        );
        assert_eq!(
            messages(&file(r#"[{"$type":"Vec2","$id":"a b"}]"#), &none),
            [(Level::Error, "inst:0".into(), "Vec2 $id \"a b\" is not an identifier".into())]
        );
        assert_eq!(
            messages(&file(r#"[{"$type":"Vec2","$id":"a"},{"$type":"Vec2","$id":"a"}]"#), &none),
            [(Level::Error, "inst:1".into(), "Vec2 a twice in this file".into())]
        );
        let other =
            Index { entries: vec![("Vec2".into(), "a".into(), PathBuf::from("/p/level2.bidata"))] };
        assert_eq!(
            messages(&file(r#"[{"$type":"Vec2","$id":"a"}]"#), &other),
            [(Level::Error, "inst:0".into(), "Vec2 a is also in level2.bidata".into())]
        );
        assert_eq!(
            messages(&file(r#"[{"$type":"Weapon","$id":"a","damage":"lots"}]"#), &none),
            [(Level::Error, "inst:0/damage".into(), "\"lots\" is not a whole number".into())]
        );
        assert_eq!(
            messages(&file(r#"[{"$type":"Weapon","$id":"a","rarity":"mythic"}]"#), &none),
            [(
                Level::Error,
                "inst:0/rarity".into(),
                "\"mythic\" is not one of common, rare, epic".into()
            )]
        );
        assert_eq!(
            messages(&file(r#"[{"$type":"Weapon","$id":"a","damage":1000}]"#), &none),
            [(Level::Warning, "inst:0/damage".into(), "1000 outside 0..999".into())]
        );
        assert_eq!(
            messages(
                &file(r#"[{"$type":"Enemy","$id":"g","weapon":"axe","drops":["axe"]}]"#),
                &none
            ),
            [
                (Level::Warning, "inst:0/weapon".into(), "no Weapon axe".into()),
                (Level::Warning, "inst:0/drops/[0]".into(), "no Weapon axe".into()),
            ]
        );
        let indexed =
            Index { entries: vec![("Weapon".into(), "axe".into(), PathBuf::from("/p/w.bidata"))] };
        assert_eq!(
            messages(&file(r#"[{"$type":"Enemy","$id":"g","weapon":"axe"}]"#), &indexed),
            []
        );
        assert_eq!(
            messages(&file(r#"[{"$type":"Enemy","$id":"g"}]"#), &none),
            [(Level::Warning, "inst:0/weapon".into(), "ref<Weapon> not set".into())]
        );
        assert_eq!(
            messages(&file(r#"[{"$type":"Weapon","$id":"a","dmg":3}]"#), &none),
            [(Level::Warning, "inst:0/?dmg".into(), "unknown key dmg".into())]
        );
        assert_eq!(
            messages(&file(r#"[{"$type":"Weapon","$id":"a","offset":{"z":1}}]"#), &none),
            [(Level::Warning, "inst:0/offset/z".into(), "unknown key z".into())]
        );
    }

    #[test]
    fn normalise_drops_defaults_orders_keys_and_keeps_unknown_ones() {
        let (_, _, schema) = both();
        let doc: Value = serde_json::from_str(
            r#"{"$dialect":"bi/1","$schema":"game.bischema","instances":[{"dmg":3,"$id":"a","rarity":"common","damage":12,"$type":"Weapon","offset":{"y":0,"x":0.5},"tags":[],"two_handed":true,"$note":"kept"}]}"#,
        )
        .unwrap();
        assert_eq!(
            write_data(&normalise(&doc, &schema)),
            "{\n  \"$dialect\": \"bi/1\",\n  \"$schema\": \"game.bischema\",\n  \"instances\": [\n    {\n      \"$type\": \"Weapon\",\n      \"$id\": \"a\",\n      \"$note\": \"kept\",\n      \"damage\": 12,\n      \"two_handed\": true,\n      \"dmg\": 3\n    }\n  ]\n}\n"
        );
        let nested: Value = serde_json::from_str(
            r#"{"$dialect":"bi/1","$schema":"s","instances":[{"$type":"Weapon","$id":"a","offset":{"x":0,"y":1}}]}"#,
        )
        .unwrap();
        // A stored key equal to the struct's own default goes; one equal
        // to the field's partial default stays, since an omitted key
        // falls back to the struct's default, not the field's.
        assert_eq!(
            normalise(&nested, &schema)["instances"][0]["offset"],
            serde_json::json!({"y": 1})
        );
        let partial: Value = serde_json::from_str(
            r#"{"$dialect":"bi/1","$schema":"s","instances":[{"$type":"Weapon","$id":"a","offset":{"x":0.5,"y":1}}]}"#,
        )
        .unwrap();
        assert_eq!(
            normalise(&partial, &schema)["instances"][0]["offset"],
            serde_json::json!({"x": 0.5, "y": 1})
        );
    }

    #[test]
    fn the_example_normalises_to_itself() {
        let (doc, _, schema) = both();
        assert_eq!(write_data(&normalise(&doc, &schema)), DATA);
    }

    #[test]
    fn renaming_an_id_rewrites_every_ref_and_the_id() {
        let (mut doc, _, schema) = both();
        assert!(rename_id(&mut doc, &schema, "Weapon", "rusty_sword", "iron_sword"));
        let text = write_data(&doc);
        assert!(!text.contains("rusty_sword"));
        assert_eq!(text.matches("iron_sword").count(), 3);
        assert!(!rename_id(&mut doc, &schema, "Weapon", "rusty_sword", "iron_sword"));
        // An Enemy called goblin is not a Weapon called goblin.
        assert!(!rename_id(&mut doc, &schema, "Weapon", "goblin", "x"));
    }

    #[test]
    fn renaming_a_field_or_an_enum_value_rewrites_the_data() {
        let (mut doc, _, schema) = both();
        assert!(rename_field(&mut doc, &schema, "Weapon", "damage", "dmg"));
        assert_eq!(doc["instances"][0]["dmg"], serde_json::json!(12));
        assert!(doc["instances"][0].get("damage").is_none());
        let keys: Vec<&String> = doc["instances"][0].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["$type", "$id", "name", "dmg", "tags"], "kept its place");
        assert!(rename_field(&mut doc, &schema, "Vec2", "x", "u"), "embedded structs too");
        assert_eq!(doc["instances"][1]["offset"], serde_json::json!({"u": 0.25, "y": -0.1}));
        assert!(rename_enum_value(&mut doc, &schema, "Rarity", "rare", "uncommon"));
        assert_eq!(doc["instances"][1]["rarity"], serde_json::json!("uncommon"));
        assert!(!rename_enum_value(&mut doc, &schema, "Rarity", "rare", "uncommon"));
    }

    #[test]
    fn prune_and_referrers() {
        let (mut doc, data, schema) = both();
        assert_eq!(
            referrers(&data, &schema, "Weapon", "rusty_sword"),
            [("Enemy".to_string(), "goblin".to_string())]
        );
        assert_eq!(
            referrers(&data, &schema, "Enemy", "goblin_chief"),
            [("Enemy".to_string(), "goblin".to_string())]
        );
        assert_eq!(referrers(&data, &schema, "Weapon", "nothing"), []);
        doc["instances"][0]["old"] = serde_json::json!(1);
        doc["instances"][3]["older"] = serde_json::json!(2);
        assert_eq!(prune(&mut doc, &schema), 2);
        assert_eq!(write_data(&doc), DATA);
        assert_eq!(type_used(&schema, "Vec2"), Some("Weapon.offset".into()));
        assert_eq!(type_used(&schema, "Enemy"), Some("Enemy.leader".into()));
        assert_eq!(type_used(&schema, "Nothing"), None);
    }

    #[test]
    fn the_index_answers_across_files() {
        let files = vec![
            (PathBuf::from("/p/level1.bidata"), DATA.to_string()),
            (PathBuf::from("/p/bad.bidata"), "{".into()),
        ];
        let index = Index::build(&files);
        assert!(index.has("Weapon", "dagger"));
        assert!(!index.has("Enemy", "dagger"));
        assert_eq!(index.ids_of("Enemy").collect::<Vec<_>>(), ["goblin", "goblin_chief"]);
        assert_eq!(index.files(), [PathBuf::from("/p/level1.bidata")]);
    }

    #[test]
    fn project_files_finds_the_schemas_data_files() {
        let dir = std::env::temp_dir().join(format!("bi-props-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("levels")).unwrap();
        std::fs::write(dir.join("game.bischema"), SCHEMA).unwrap();
        std::fs::write(
            dir.join("levels/level1.bidata"),
            DATA.replace("game.bischema", "../game.bischema"),
        )
        .unwrap();
        std::fs::write(dir.join("level0.bidata"), DATA).unwrap();
        std::fs::write(dir.join("other.bidata"), DATA.replace("game.bischema", "other.bischema"))
            .unwrap();
        std::fs::write(dir.join("notes.json"), DATA).unwrap();
        let found =
            project_files(&dir, &dir.join("game.bischema"), Some(&dir.join("level0.bidata")));
        assert_eq!(found, [dir.join("levels/level1.bidata")]);
        let all = project_files(&dir, &dir.join("game.bischema"), None);
        assert_eq!(all, [dir.join("level0.bidata"), dir.join("levels/level1.bidata")]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
