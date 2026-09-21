# Property view

Data-driven properties: a `.bischema` defines struct-like types, a
`.bidata` holds typed instances of them, and both are JSON. A text editor
shows the JSON; what a designer wants is Unity's inspector — every field of
an instance on its own row, defaults dimmed, enums cycled, refs resolved,
and a number turned without retyping the line around it. `:set editor
bidata` (or the extension) draws that view of the file. The file stays the
file; the view is a view of it. The format itself is
`docs/specs/bi-format.md`.

## Status

**Built.** Both modes, editing, validation, refs across the project, and
the `:bi` refactorings — `new`, `delete`, `add`, `rename`, `remap`,
`prune`, `migrate`.

## What it looks like

A `.bidata` opens as a tree of instances; a `.bischema` as a tree of types.
One pane, in the window the file opened in, with the file's status row:

```
▾ Weapon rusty_sword
    name        "Rusty Sword"
    damage      12
    rarity      common                    ← dimmed: the default
  ▸ offset      { x 0.5, y 0 }            ← dimmed, a struct
    tags        [melee, starter]
    two_handed  false
▸ Weapon dagger
▾ Enemy goblin
    name        "Goblin"
    hp          40
    speed       1.4
    weapon      rusty_sword  → Weapon
  ▾ drops       2 × ref<Weapon>
      [0]       rusty_sword
      [1]       dagger
  ▸ spawn       { x 12, y 3 }
    leader      goblin_chief  ⚠ no Enemy goblin_chief
    damage      99            ⚠ unknown key
```

```
▾ Rarity  enum        Drop tier, drives colour and loot tables
    common
    rare
    epic
▾ Weapon  struct
  ▸ name        string
  ▾ damage      i32 = 10  0..999
      type      i32
      default   10
      min       0
      max       999
      step      —
      doc       —
```

A set value is drawn plainly, an inherited one in the dim colour; a
warning rides on its row after `⚠`. A row's `doc` is the status message
while it is selected.

**Errors block the view**, as the format says: a file that fails to parse
or validate opens in text view with the error in the status. A view
already open whose text is broken by hand — in a text split beside it —
goes blank with the error as its one row until the text is fixed.

## Opening

The extension is the trigger and `$dialect` the confirmation:

- `bi level1.bidata`, `:e level1.bidata`, `Enter` on it in the tree, the
  file picker — every path a file opens by — reads the extension, opens
  the text buffer as ever, and puts the property view over it.
- `:set editor bischema` and `:set editor bidata` on a text window do the
  same for a file without the extension; `:set editor text` on the view
  puts the text back. `:set editor` alone says which is up.
- A `.bidata` names its schema by `$schema`, relative to itself; a schema
  that is open in a buffer is read from the buffer, not the disk, so an
  unsaved change to a field's default already shows in the data.

## The keys

The pane borrows the tree's keymap for what it does not claim, so `Ctrl-W`
and the leader still work, and reads its own vocabulary on top:

```
j  k  ↓ ↑   Ctrl-D  Ctrl-U   pick a row; counts multiply      gg  G   first, last
l  →  Enter  open a struct, a list, a type or a field's attributes
h  ←         close it, or go to the parent row
Enter  i     on a value: edit it on the ex line — `:bi set goblin.hp 40`
Space        flip a bool, cycle an enum, a ref through the ids of its type,
             an optional between — and the inner value's default
Ctrl-A  Ctrl-X   a number up, down by its step, clamped to min..max;
             an enum or a ref to the next, previous; counts multiply
a            add: an item to a list, an instance to a data file, a field to a
             struct, a value to an enum, a type to a schema — the ones that
             need a name prefill the ex line
dd           remove: a set value goes back to its default, a list item goes,
             an unknown key goes, an instance goes — one other instances
             reference prefills `:bi delete Enemy goblin` instead, and that
             line is the confirmation — a field or value leaves the schema
gd           on a ref: jump to its target, in this file or another
u  Ctrl-R    undo, redo — the buffer's; the view follows
:            the ex line
```

Every key that changes something is one edit of the buffer and one undo
step of it; the view re-reads the buffer after, so there is one direction
of data flow and no second history.

## Paths

The ex forms name a row by a path, the same string `Enter` prefills.

In a data file: `<id>.<field>`, deeper with `.` for a struct's field and
`[n]` for a list item — `goblin.hp`, `dagger.offset.x`, `goblin.drops[1]`.
Ids are per type, so an id two types share is written `Enemy:goblin`. The
instance's own id is `goblin.$id`; setting it rewrites every ref to it in
every data file of the schema, which is what the format promises.

In a schema: `<Type>.<field>.<attr>` with `type`, `default`, `min`, `max`,
`step`, `doc` as the attributes, `<Type>.<field>.name` to rename the field,
`<Type>.doc` for the type's own line, `<Enum>.values[n]` for a value.
Renaming a field or an enum value through `set` is the refactoring: every
data file of the schema is rewritten in the same step.

## Values on the ex line

`:bi set <path> <value>` parses `<value>` by the field's type: `true` and
`false`; a number, whole for the integer types; a string as typed, quotes
optional and JSON escapes honoured inside them; an enum by name; a ref by
id; `null`, `none` or `-` for an absent optional; a list or a struct as
JSON. A value that does not parse is refused naming what the field takes;
a number outside `min..max` is written and warned about, as the format
says. `:bi set <path>` with no value reports it.

Writing a value equal to the default removes the key: the row goes dim
and the file loses a line, which is what sparse storage means. A list or
struct value is materialised whole from the resolved value before an item
inside it is changed, so `goblin.drops[1]` on an inherited list writes the
list.

## `:bi`

```
:bi set <path> [value]           the value, or a report
:bi new <Type> <id>              a new instance at the end of a data file, required refs blank
:bi delete <Type> <id>           an instance, however many refs it has; they dangle and warn
:bi add <Type> <field> <type>    a field on a struct — `:bi add Weapon speed f32`
:bi add <Enum> <value>           a value on an enum
:bi add <Name> struct|enum       a type
:bi rename <Type>.<old> <new>    a field, an enum value (schema) or an id (data), data files rewritten
:bi remap <Type>.<old> <new>     an unknown key renamed across the data files — a rename made by hand
:bi prune                        every unknown key of this file removed
:bi migrate                      an older `$dialect` brought up to bi/1
```

Each refuses with a message when the path does not exist or the name is
taken. `:bi` alone lists them.

**The project's data files** are every `.bidata` under the project root
(the tree's root, else the schema's directory) whose `$schema` resolves to
the schema in hand, ignoring what git ignores. The index over them is
what checks duplicate ids and dangling refs, what `Space` cycles a ref
through, what `gd` jumps by, and what the refactorings rewrite. A file
that is open in a buffer is read and rewritten in the buffer, as one undo
step of that buffer; one that is not is rewritten on disk. The index of
the other files is built when the view opens and after every `:w` and
refactoring; this file's part after every edit.

## Writing

`:w` on the view normalises and writes. Normalising is the format's: in a
data file a key whose value equals its default goes; keys come out in the
schema's order with `$type` and `$id` first and unknown keys after; in a
schema, definitions come out one property per line. Layout is the
editor's: two-space indent, one property per line at the top, in `types`,
in each definition, in `fields`, in `instances` and in each instance;
everything nested deeper inline. Every edit the view makes writes the same
layout, so the first edit of a hand-written file reformats it once and
never again. Unknown keys are kept everywhere.

## The model

`src/props/`, with no editor in it:

```rust
// mod.rs
pub enum Kind { Schema, Data }
pub fn kind_of(path: &Path) -> Option<Kind>;          // by extension
pub fn check_dialect(doc: &Value) -> Result<(), Dialect>;   // Missing, Unknown, Older(n), Newer(n)
pub struct Diagnostic { pub level: Level, pub at: Option<String>, pub message: String }
pub fn write_schema(doc: &Value) -> String;           // the layout above
pub fn write_data(doc: &Value) -> String;

// schema.rs
pub enum TypeExpr { Bool, I32, I64, F32, F64, Str, Named(String), List(Box<..>), Optional(Box<..>), Ref(String) }
pub struct FieldDef { name, ty, default: Option<Value>, min, max, step, doc }
pub enum TypeDef { Struct { fields, doc }, Enum { values, doc } }
pub struct Schema { types: Vec<(String, TypeDef)> }
impl Schema {
    pub fn parse(text: &str) -> Result<(Value, Schema), Vec<Diagnostic>>;
    pub fn default_of(&self, ty: &TypeExpr) -> Value;
    pub fn field_default(&self, field: &FieldDef) -> Value;   // the partial default over the struct's
    pub fn check(&self, ty: &TypeExpr, value: &Value) -> Result<(), String>;
    pub fn parse_value(&self, ty: &TypeExpr, text: &str) -> Result<Value, String>;
}

// data.rs
pub struct Instance { pub ty: String, pub id: String, pub values: Map }
pub struct DataFile { pub schema: String, pub instances: Vec<Instance> }
pub struct Index { ids: Vec<(String, String, PathBuf)> }     // (type, id, file) over the other files
pub fn parse(text: &str) -> Result<(Value, DataFile), Vec<Diagnostic>>;
pub fn validate(data: &DataFile, schema: &Schema, index: &Index) -> Vec<Diagnostic>;
pub fn normalise(doc: &Value, schema: &Schema) -> Value;
pub fn project_files(root: &Path, schema: &Path) -> Vec<PathBuf>;

// view.rs
pub struct Props { kind, buffer, path, schema_path, raw: Value, schema: Option<Schema>,
                   data: Option<DataFile>, diagnostics, index, rows: Vec<Row>,
                   selected, scroll, expanded: BTreeSet<String>, error: Option<String>,
                   seen: Option<u64> }
pub struct Row { pub key: String, pub depth: usize, pub label: String, pub value: String,
                 pub kind: RowKind, pub inherited: bool, pub warning: Option<String>,
                 pub doc: Option<String>, pub expandable: bool, pub expanded: bool }
```

The raw `Value` — `serde_json` with `preserve_order` — is the document;
the parsed `Schema` and `DataFile` are read from it for validation and
row building, and every edit is a change to the raw value followed by
`write_*`, so unknown keys on definitions and instances survive without
anyone listing them. An edit returns the new text; the editor puts it in
the buffer as one undo step and the view re-reads on the next sync, the
curve tool's arrangement.

Rows are rebuilt from the document and the `expanded` set after every
change; selection and expansion are held by row key, so an edit that
reorders nothing keeps the cursor where it was, and one that removes the
selected row lands on the nearest.

## The view in the editor

`Content::Props(Box<Props>)` on a window, for the tree's reason: the
selected row and what is open are view state. `Content::buffer` reports
the buffer, so `:w`, `:q`, `:bd`, the modified marker and the buffer list
see the file as the file it is. The pane's key grammar is `PropsCmd`,
read in `input.rs` beside the form's; the after-key sync re-reads any
props window whose buffer's edit counter moved.

## Tests

- `TypeExpr::parse` reads every primitive, a name, and nested generics;
  refuses whitespace, an unknown generic, `ref<i32>`,
  `optional<optional<T>>`.
- `Schema::parse` on the full example finds four types; every schema
  error in the format's table is reported with its type or field named.
- `default_of` gives the table's implicit defaults; a partial struct
  default fills from the struct's own; `parse_value` reads each encoding
  and refuses with what the type wants.
- `data::parse` plus `validate` on the full example is clean; each data
  error and warning in the table is produced, with the instance and field
  named; a ref found only through the index is not dangling; a duplicate
  id in another file is an error.
- `normalise` drops a value equal to its default, keeps an override,
  orders keys, keeps unknown keys after.
- `write_data` on the example round-trips its layout byte for byte;
  `write_schema` the same.
- Rows: the example expands to the tree above; inherited rows are dimmed;
  a dangling ref carries its warning; the `expanded` set survives a
  rebuild.
- Edits: `set goblin.hp 41` rewrites one line; `set rusty_sword.rarity
  common` removes the key; `set goblin.drops[1] warhammer` writes the
  list; `set rusty_sword.$id iron_sword` rewrites three places; `Space`
  on a bool, an enum, a ref, an optional; `Ctrl-A` clamps at `max`; `dd`
  on a set value, a list item, an unknown key, an instance; `a` on a list;
  `new`, `delete`, `add`, `rename`, `remap`, `prune` each as one undo
  step; a bad value is refused naming the type.
- Opening: a `.bidata` opens in the view; a missing `$dialect` opens as
  text with the error; `:set editor text` and back; a schema edited by
  hand in a split updates the data view; `:w` normalises and writes; `u`
  restores the text and the view.
