//! The small schema and data every backend test generates from, and the
//! helpers that turn text into a model.

use super::Lang;
use super::mapping::Mapping;
use super::model::Model;
use crate::props::Diagnostic;
use crate::props::data::{self, DataFile};
use crate::props::schema::Schema;

pub const STEM: &str = "weapons";
pub const SOURCE: &str = "weapons.bischema";

pub const SCHEMA: &str = r##"{
  "$dialect": "bi/1",
  "types": {
    "Rarity": { "kind": "enum", "values": ["common", "very rare"], "doc": "Tier" },
    "Vec2": { "kind": "struct", "fields": [
      { "name": "x", "type": "f32", "default": 0 },
      { "name": "y", "type": "f32", "default": 0 }
    ]},
    "Weapon": { "kind": "struct", "doc": "A thing", "fields": [
      { "name": "name", "type": "string", "default": "Sword \"x\"" },
      { "name": "damage", "type": "i32", "default": 10, "min": 0, "max": 999 },
      { "name": "rarity", "type": "Rarity" },
      { "name": "offset", "type": "Vec2", "default": { "x": 0.5 } },
      { "name": "tags", "type": "list<string>", "default": ["a"] },
      { "name": "notes", "type": "optional<string>" },
      { "name": "tint", "type": "rgb", "default": "#c8c8c8" },
      { "name": "owner", "type": "ref<Weapon>" },
      { "name": "type", "type": "bool" }
    ]}
  }
}"##;

pub const DATA: &str = r##"{
  "$dialect": "bi/1", "$schema": "weapons.bischema",
  "instances": [
    { "$type": "Weapon", "$id": "rusty_sword", "owner": "rusty_sword" },
    { "$type": "Weapon", "$id": "dagger", "owner": "rusty_sword" }
  ]
}"##;

/// A cycle through optional: Node { next: optional<Node>, kids: list<Node> }.
pub const CYCLE: &str = r##"{ "$dialect": "bi/1", "types": {
  "Node": { "kind": "struct", "fields": [
    { "name": "next", "type": "optional<Node>" },
    { "name": "kids", "type": "list<Node>" }
  ]}}}"##;

/// A schema with every builtin: the support file carries all five.
pub const EVERY_BUILTIN: &str = r##"{ "$dialect": "bi/1", "types": {
  "Fx": { "kind": "struct", "fields": [
    { "name": "tint", "type": "rgb" },
    { "name": "glow", "type": "rgba", "default": "#ff000080" },
    { "name": "falloff", "type": "curve" },
    { "name": "trail", "type": "gradient" },
    { "name": "next", "type": "optional<ref<Fx>>" },
    { "name": "rows", "type": "list<list<f32>>", "default": [[1, 2], []] },
    { "name": "big", "type": "u64", "default": 18446744073709551615 }
  ]}}}"##;

pub fn try_model(
    lang: Lang,
    schema: &str,
    data: &[&str],
    mapping: &str,
) -> Result<(Model, Vec<Diagnostic>), Vec<Diagnostic>> {
    let (_, schema) = Schema::parse(schema).expect("fixture schema parses");
    let data: Vec<(String, String, DataFile)> = data
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let stem = if i == 0 { "level1".to_string() } else { format!("level{}", i + 1) };
            (stem.clone(), format!("{stem}.bidata"), data::parse(d).expect("fixture data parses").1)
        })
        .collect();
    let mapping = Mapping::parse(mapping).expect("fixture mapping parses");
    Model::build(
        lang,
        vec![(STEM.into(), SOURCE.into(), schema, data)],
        &mapping.for_lang(lang),
        mapping.ids,
        mapping.instances,
    )
}

pub fn model_notes(
    lang: Lang,
    schema: &str,
    data: &[&str],
    mapping: &str,
) -> (Model, Vec<Diagnostic>) {
    try_model(lang, schema, data, mapping).unwrap_or_else(|e| panic!("{e:?}"))
}

pub fn model_of(lang: Lang, schema: &str, data: &[&str], mapping: &str) -> Model {
    model_notes(lang, schema, data, mapping).0
}

/// The fixture schema, with its data file when `with_data`, under `mapping`.
pub fn model(lang: Lang, with_data: bool, mapping: &str) -> Model {
    let data: &[&str] = if with_data { &[DATA] } else { &[] };
    model_of(lang, SCHEMA, data, mapping)
}
