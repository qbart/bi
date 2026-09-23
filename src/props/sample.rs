//! A sample schema and data file: every type expression, every layout
//! attribute, sparse storage, refs and an optional, in the writer's own
//! layout. `:bi sample` writes them into a buffer, and the same texts sit
//! in `examples/props/` for reading. See `docs/specs/props.md`.

pub const SCHEMA_NAME: &str = "game.bischema";
pub const DATA_NAME: &str = "level1.bidata";

pub const SCHEMA: &str = r##"{
  "$dialect": "bi/1",
  "types": {
    "Rarity": {
      "kind": "enum",
      "values": ["common", "rare", "epic", "legendary"],
      "doc": "Drop tier, drives colour and loot tables"
    },
    "Element": {
      "kind": "enum",
      "values": ["none", "fire", "ice", "lightning"]
    },
    "Vec2": {
      "kind": "struct",
      "fields": [
        { "name": "x", "type": "f32", "default": 0 },
        { "name": "y", "type": "f32", "default": 0 }
      ]
    },
    "Stats": {
      "kind": "struct",
      "doc": "The numbers every living thing has",
      "fields": [
        { "name": "hp", "type": "i32", "default": 100, "min": 1, "max": 9999 },
        { "name": "armor", "type": "u8", "default": 0, "min": 0, "max": 100 },
        { "name": "speed", "type": "f32", "default": 1.0, "min": 0, "max": 10, "step": 0.1 }
      ]
    },
    "Weapon": {
      "kind": "struct",
      "doc": "A thing to hit with",
      "groups": { "Advanced": { "collapsed": true, "doc": "Rarely tuned by hand" } },
      "fields": [
        { "name": "name", "type": "string", "doc": "Shown in the inventory" },
        { "name": "damage", "type": "i32", "default": 10, "min": 0, "max": 999, "group": "Combat" },
        { "name": "element", "type": "Element", "default": "none", "group": "Combat", "widget": "toggle" },
        { "name": "crit_chance", "type": "f32", "default": 0.05, "min": 0, "max": 1, "step": 0.01, "group": "Combat", "label": "Crit chance" },
        { "name": "crit_multiplier", "type": "f32", "default": 2, "min": 1, "max": 5, "step": 0.1, "group": "Combat", "label": "Crit multiplier", "show_if": "crit_chance > 0" },
        { "name": "rarity", "type": "Rarity", "default": "common" },
        { "name": "two_handed", "type": "bool", "default": false },
        { "name": "tags", "type": "list<string>" },
        { "name": "tint", "type": "rgb", "default": "#c8c8c8", "doc": "The sprite's colour" },
        { "name": "glow", "type": "rgba", "group": "Advanced" },
        { "name": "falloff", "type": "curve", "group": "Combat", "doc": "Damage over the swing, 0..1" },
        { "name": "trail", "type": "gradient", "group": "Advanced", "doc": "The swing's trail, start to end" },
        { "name": "offset", "type": "Vec2", "default": { "x": 0.5 }, "group": "Advanced", "widget": "inline" },
        { "name": "id_hash", "type": "u32", "group": "Advanced", "readonly": true, "doc": "Assigned by the build" },
        { "name": "notes", "type": "optional<string>", "group": "Advanced" }
      ]
    },
    "Enemy": {
      "kind": "struct",
      "fields": [
        { "name": "name", "type": "string", "order": -1 },
        { "name": "stats", "type": "Stats", "widget": "inline" },
        { "name": "weapon", "type": "ref<Weapon>" },
        { "name": "drops", "type": "list<ref<Weapon>>" },
        { "name": "spawn", "type": "Vec2" },
        { "name": "leader", "type": "optional<ref<Enemy>>" },
        { "name": "boss", "type": "bool", "default": false, "group": "Boss" },
        { "name": "boss_phases", "type": "u8", "default": 1, "min": 1, "max": 3, "group": "Boss", "show_if": "boss" },
        { "name": "boss_music", "type": "optional<string>", "group": "Boss", "hide_if": "!boss" }
      ]
    }
  }
}
"##;

pub const DATA: &str = r##"{
  "$dialect": "bi/1",
  "$schema": "game.bischema",
  "instances": [
    {
      "$type": "Weapon",
      "$id": "rusty_sword",
      "name": "Rusty Sword",
      "damage": 12,
      "tags": ["melee", "starter"],
      "id_hash": 3141
    },
    {
      "$type": "Weapon",
      "$id": "dagger",
      "name": "Dagger",
      "damage": 7,
      "crit_chance": 0.25,
      "crit_multiplier": 3,
      "rarity": "rare",
      "tags": ["melee", "fast"],
      "offset": { "x": 0.25, "y": -0.1 },
      "id_hash": 2718
    },
    {
      "$type": "Weapon",
      "$id": "frost_hammer",
      "name": "Frost Hammer",
      "damage": 40,
      "element": "ice",
      "crit_chance": 0,
      "rarity": "epic",
      "two_handed": true,
      "tint": "#7fd4ff",
      "glow": "#7fd4ff80",
      "falloff": [[0, 0.2, 0, 0, false], [0.6, 1, 0, 0, true], [1, 0.4, -1.5, -1.5, true]],
      "trail": [[0, "#7fd4ffff"], [0.7, "#7fd4ff80"], [1, "#7fd4ff00"]],
      "id_hash": 1618,
      "notes": "Slows on hit; see the status effects table"
    },
    {
      "$type": "Enemy",
      "$id": "goblin",
      "name": "Goblin",
      "stats": { "hp": 40, "speed": 1.4 },
      "weapon": "rusty_sword",
      "drops": ["rusty_sword", "dagger"],
      "spawn": { "x": 12, "y": 3 },
      "leader": "goblin_chief"
    },
    {
      "$type": "Enemy",
      "$id": "goblin_chief",
      "name": "Goblin Chief",
      "stats": { "hp": 120, "armor": 10 },
      "weapon": "frost_hammer",
      "drops": ["frost_hammer"],
      "spawn": { "x": 20, "y": 3 },
      "boss": true,
      "boss_phases": 2,
      "boss_music": "chief_theme.ogg"
    }
  ]
}
"##;

#[cfg(test)]
mod tests {
    use super::super::data::{Index, parse, validate};
    use super::super::schema::Schema;
    use super::super::{write_data, write_schema};
    use super::*;

    #[test]
    fn the_sample_reads_clean_and_round_trips() {
        let (raw, schema) = Schema::parse(SCHEMA).unwrap();
        assert_eq!(write_schema(&raw), SCHEMA, "the writer's own layout");
        let (raw, data) = parse(DATA).unwrap();
        assert_eq!(write_data(&raw), DATA);
        assert_eq!(validate(&data, &schema, &Index::default()), Vec::new());
        assert!(schema.field("Weapon", "crit_multiplier").unwrap().show_if.is_some());
        assert!(schema.field("Enemy", "boss_music").unwrap().hide_if.is_some());
        assert!(schema.field("Weapon", "id_hash").unwrap().readonly);
    }

    /// The files under `examples/props/` are the same texts, so what the
    /// docs point at is what `:bi sample` writes.
    #[test]
    fn the_example_files_are_the_sample() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/props");
        assert_eq!(std::fs::read_to_string(dir.join(SCHEMA_NAME)).unwrap(), SCHEMA);
        assert_eq!(std::fs::read_to_string(dir.join(DATA_NAME)).unwrap(), DATA);
    }
}
