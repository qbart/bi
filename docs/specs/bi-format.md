# bi property format (bi/1)

The bi format is a JSON dialect for defining struct-like types and creating
typed instances of them, edited in the property view (`props.md`).
Everything is valid JSON, so `jq`, `git diff` and any JSON loader keep
working; the rules below only say what the keys mean.

Two file kinds:

| Kind   | Holds                                       | Extension   |
|--------|---------------------------------------------|-------------|
| Schema | Type definitions (structs, enums)           | `.bischema` |
| Data   | Instances of schema types with concrete values | `.bidata` |

A data file points at one schema file. A schema file is self-contained (no
imports in bi/1). Instances are stored sparsely: a field that is omitted
takes its default from the schema. Files are UTF-8, `\n` line endings,
2-space indent and one property per line when written by the editor (the
editor round-trips its own layout; hand-written layout is accepted on
read).

## Extensions and the `$dialect` header

The extension is the fast trigger (property view on open, file-picker
filters, globs); `$dialect` is the confirmation and the version. Both are
required.

- `*.bischema` — schema file. Opens in property view in schema mode.
- `*.bidata` — data file. Opens in property view in instance mode.

Every file's first key is `"$dialect": "bi/1"`. `bi` names the format, `1`
is the major format version. The editor:

1. Picks the view from the extension.
2. Parses the file. Missing or unknown `$dialect` → text view plus an
   error; the file is never reinterpreted by guessing.
3. Compares the version. Older major version → offer migration; newer →
   text view plus an error.

All top-level and per-object keys starting with `$` are reserved for bi
metadata. Types, fields and instances must not use `$`-prefixed names.

Add this to `.gitattributes` in repos that hold bi files so hosting tools
highlight them as JSON:

```
*.bischema linguist-language=JSON
*.bidata   linguist-language=JSON
```

## Schema file (`.bischema`)

One JSON object with `$dialect` and a `types` map. Each key in `types` is
a type name; each value is a type definition with a `kind`.

| Top-level key | Required | Value                                |
|---------------|----------|--------------------------------------|
| `$dialect`    | yes      | `"bi/1"`                             |
| `types`       | yes      | Object: type name → type definition  |

Type names are identifiers (`[A-Za-z_][A-Za-z0-9_]*`), unique within the
file, and may not shadow a primitive (`bool`, `i32`, …) or a built-in
generic (`list`, `optional`, `ref`). PascalCase is the convention. Type
definitions may reference each other in any order; recursion through
`ref<T>` or `list<T>` is allowed, direct recursion by embedding (a struct
containing itself as a plain field) is an error.

### struct

| Key      | Required | Value                                  |
|----------|----------|----------------------------------------|
| `kind`   | yes      | `"struct"`                             |
| `fields` | yes      | Array of field objects, in display order |
| `doc`    | no       | One-line description shown in the editor |

Fields are an array, not an object, because order is meaningful. Each
field object:

| Key                 | Required | Value                                                   |
|---------------------|----------|---------------------------------------------------------|
| `name`              | yes      | Identifier, unique within the struct; the key used in data files |
| `type`              | yes      | Type expression (next section)                          |
| `default`           | no       | Value in that type's data encoding; replaces the implicit default |
| `min`, `max`, `step`| no       | Numbers; only on numeric fields (`i32`, `i64`, `f32`, `f64`) |
| `doc`               | no       | One-line description                                    |

The name is the field's identity: data files store values by name, and
nothing else refers to a field. Renaming is therefore an editor action
(`:bi rename Weapon.damage dmg`) that rewrites every data file in the same
step. A rename made outside the editor (text view, git) shows up as a
deleted field plus a new one, and existing values surface as unknown-key
warnings; `:bi remap Weapon.damage dmg` fixes those after the fact. Field
order in the array is display order only and may change freely.

### enum

| Key      | Required | Value                            |
|----------|----------|----------------------------------|
| `kind`   | yes      | `"enum"`                         |
| `values` | yes      | Non-empty array of unique strings |
| `doc`    | no       | One-line description             |

Enum values are stored by name, never by index, so reordering values does
not change data. Renaming a value is a refactoring that rewrites data
files.

A minimal complete schema:

```json
{
  "$dialect": "bi/1",
  "types": {
    "Rarity": {
      "kind": "enum",
      "values": ["common", "rare", "epic"]
    },
    "Weapon": {
      "kind": "struct",
      "fields": [
        { "name": "name",   "type": "string" },
        { "name": "damage", "type": "i32", "default": 10, "min": 0, "max": 999 },
        { "name": "rarity", "type": "Rarity", "default": "common" }
      ]
    }
  }
}
```

## Type expressions

A field's type is a string parsed by this grammar; whitespace is not
allowed inside it:

```
type      := primitive | name | generic
generic   := ( "list" | "optional" | "ref" ) "<" type ">"
name      := identifier             (a key of `types`)
primitive := "bool" | "i32" | "i64" | "f32" | "f64" | "string"
```

Parse every type string once at schema load into a tree and validate each
leaf name against the primitives plus the keys of `types`; an unknown name
is a schema error.

| Expression       | Meaning                              | Data encoding                    | Implicit default |
|------------------|--------------------------------------|----------------------------------|------------------|
| `bool`           | boolean                              | `true` / `false`                 | `false`          |
| `i32`, `i64`     | signed integer                       | JSON number without fraction     | `0`              |
| `f32`, `f64`     | float                                | JSON number                      | `0`              |
| `string`         | UTF-8 text                           | JSON string                      | `""`             |
| `E` (enum name)  | one of `E.values`                    | JSON string equal to a value     | first value      |
| `S` (struct name)| embedded value, owned by the parent  | JSON object with `S`'s fields    | object of `S`'s defaults |
| `list<T>`        | ordered sequence                     | JSON array of `T` encodings      | `[]`             |
| `optional<T>`    | value or absent                      | `null` or `T`'s encoding         | `null`           |
| `ref<S>`         | reference to an instance of struct `S` | JSON string holding the target's `$id` | none: required (see below) |

Constraints on nesting: `ref<T>` requires `T` to be a struct type (not a
primitive, enum, list or optional). `optional<optional<T>>` is an error.
Everything else composes freely: `list<ref<Weapon>>`, `optional<list<i32>>`,
`list<list<f32>>`.

Embedded struct vs ref: embed for small value-like data each parent owns
(`Vec2`, `Color`, a stats block); use `ref` for things with identity that
are shared, so one edit reaches every user. A `ref` field without a
default is required: an instance that omits it gets a missing required
field diagnostic, and `:bi new` fills it with an empty string that is
flagged until set. Make it `optional<ref<S>>` when absence is legitimate.

A default for a struct-typed field is a partial object: keys it omits fall
back to the struct's own defaults.

## Data file (`.bidata`)

A data file holds instances of types from exactly one schema.

| Top-level key | Required | Value                                              |
|---------------|----------|----------------------------------------------------|
| `$dialect`    | yes      | `"bi/1"`                                           |
| `$schema`     | yes      | Path to the `.bischema` file, relative to this file, forward slashes |
| `instances`   | yes      | Array of instance objects, in display order        |

Each instance object:

| Key            | Required | Value                                                   |
|----------------|----------|---------------------------------------------------------|
| `$type`        | yes      | Name of a struct type in the schema (enums cannot be instantiated) |
| `$id`          | yes      | Identifier, unique per type across all data files that share the schema |
| `<field name>` | no       | The field's value in its data encoding                  |

Ids are scoped per type: a `Weapon` and an `Enemy` may both be called
`boss`, because every `ref<S>` already names which type to look in. The
editor keeps a project-wide index of `($type, $id)` → file, rebuilt on
save, and uses it for ref completion, `gd` jump-to-target, reverse lookup
and rename.

Storage is sparse. A field that is absent from an instance has its default
(explicit default or implicit one); the editor renders inherited values
dimmed and set values normally. Writing a value equal to the default back
to the file is allowed, but the editor's `:w` normalises it by removing
the key, so `git diff` shows only real overrides. Changing a default in the
schema therefore changes every instance that did not override it.

A ref value is the target's `$id` as a string; a `list<ref<S>>` is an
array of such strings. Cycles (A refs B, B refs A) are legal, since the
file stores only ids. Loaders resolve refs in a second pass after all
instances are registered.

Unknown keys on an instance (a name not in the struct, typically left over
from a deleted or renamed field) are preserved on write and reported as
warnings; the editor never drops data silently. A `dd` on the row removes
the key explicitly.

A data file for the minimal schema above:

```json
{
  "$dialect": "bi/1",
  "$schema": "game.bischema",
  "instances": [
    {
      "$type": "Weapon",
      "$id": "rusty_sword",
      "name": "Rusty Sword",
      "damage": 12
    },
    {
      "$type": "Weapon",
      "$id": "dagger",
      "name": "Dagger",
      "damage": 7,
      "rarity": "rare"
    }
  ]
}
```

`rusty_sword` omits `rarity`, so it is `common` from the default and shown
dimmed.

## Validation and diagnostics

Errors block the property view (the file opens in text view with the error
marked); warnings show inline on the affected row and never block editing
or saving.

| Check                                                                   | Where  | Level   |
|-------------------------------------------------------------------------|--------|---------|
| Invalid JSON, missing/unknown `$dialect`, newer major version           | any    | error   |
| Missing `types` / `instances` / `$schema`, or `$schema` file not found  | file   | error   |
| Type name not an identifier, shadows a primitive or generic, duplicate  | schema | error   |
| Unknown `kind`; struct without `fields`; enum with empty or duplicate values | schema | error |
| Field name not an identifier, duplicated, or `$`-prefixed               | schema | error   |
| Type expression unparsable or names an unknown type                     | schema | error   |
| `ref<T>` where `T` is not a struct; `optional<optional<T>>`; direct embed recursion | schema | error |
| `default` not encodable as the field's type; `min`/`max`/`step` on a non-numeric field | schema | error |
| `$type` not a struct in the schema; `$id` missing or not an identifier  | data   | error   |
| Duplicate `($type, $id)` across the project                             | data   | error   |
| Value not encodable as the field's type (string in an `i32`, unknown enum value) | data | error |
| Number outside `min`/`max`                                              | data   | warning |
| ref to an id that does not exist for that type                          | data   | warning (dangling ref) |
| Required ref field absent                                               | data   | warning |
| Unknown key on an instance                                              | data   | warning (preserved) |
| Deleting an instance that other instances reference                     | editor | confirmation prompt |

The `$schema` path is resolved relative to the data file. A schema that
fails to load makes every data file that uses it open in text view, with
the schema error shown once at the top.

## Versioning and migration

`bi/1` is the major format version, and only incompatible changes bump it.
Adding an optional key (a new field attribute, a new `doc`) or a new
primitive is not a bump: readers of bi/1 must ignore unknown `$`-less keys
on type and field definitions and keep them on write.

On opening a file with an older major version the editor offers
`:bi migrate`, which rewrites the file to the current version in place;
the user reviews the result in `git diff`. A newer version is refused with
an error, never partially read.

Schema-level changes that touch data are refactorings, run by the editor
across every `.bidata` that references the schema:

| Change                        | Effect on data files                                       |
|-------------------------------|------------------------------------------------------------|
| Rename field (`:bi rename`)   | Key renamed in every instance                              |
| Rename enum value             | Value rewritten wherever stored                            |
| Rename `$id`                  | Every ref to it rewritten                                  |
| Change field type             | Values that still encode are kept; others become unknown-key warnings |
| Change default                | No data change; instances without an override pick up the new value |
| Delete field                  | Keys left in place as unknown-key warnings until removed by hand or `:bi prune` |
| Reorder fields                | No data change; array order is display order only          |

Editing the schema by hand in text view skips these refactorings; the
warnings on the next open show what drifted, and `:bi remap` reconciles a
rename after the fact. Fields carry no numeric ids in bi/1 because data is
addressed by name everywhere; if a compact binary export addressed by
field number is added later, bi/2 introduces ids and the migration assigns
them from array order.

## Full example

Two files in one directory. The schema exercises every type expression
form; the data file exercises sparse storage, embedded structs, lists,
refs and an optional.

`game.bischema`:

```json
{
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
        { "name": "name",       "type": "string" },
        { "name": "damage",     "type": "i32", "default": 10, "min": 0, "max": 999 },
        { "name": "rarity",     "type": "Rarity", "default": "common" },
        { "name": "offset",     "type": "Vec2", "default": { "x": 0.5 } },
        { "name": "tags",       "type": "list<string>" },
        { "name": "two_handed", "type": "bool", "default": false }
      ]
    },
    "Enemy": {
      "kind": "struct",
      "fields": [
        { "name": "name",   "type": "string" },
        { "name": "hp",     "type": "i32", "default": 100, "min": 1 },
        { "name": "speed",  "type": "f32", "default": 1.0, "min": 0, "max": 10, "step": 0.1 },
        { "name": "weapon", "type": "ref<Weapon>" },
        { "name": "drops",  "type": "list<ref<Weapon>>" },
        { "name": "spawn",  "type": "Vec2" },
        { "name": "leader", "type": "optional<ref<Enemy>>" }
      ]
    }
  }
}
```

`level1.bidata`:

```json
{
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
```

How the editor reads it: `rusty_sword` shows `rarity` common, `offset`
(0.5, 0) and `two_handed` false dimmed, all from defaults, with `offset.x`
coming from the field default and `offset.y` from `Vec2`'s own default.
`goblin_chief` has no `leader` key, so it is null and shown dimmed as `—`.
Every ref in the file resolves; renaming `rusty_sword` to `iron_sword` in
property view rewrites three places (`goblin.weapon`, `goblin.drops[0]`,
and the `$id` itself).

A loader does two passes: register every instance under `($type, $id)`
with defaults applied, then walk ref fields and swap each id string for a
handle. The two-pass order is what makes the goblin → chief → warhammer
chain and any cycles safe.
