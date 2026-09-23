# Property view

Data-driven properties: a `.bischema` defines struct-like types, a
`.bidata` holds typed instances of them, and both are JSON. A text editor
shows the JSON; what a designer wants is Unity's inspector with Odin on
it — every field of an instance as a widget, sliders and checkboxes and
choices, grouped and ordered and hidden as the type says, defaults dimmed,
refs resolved. `:set editor bidata` (or the extension) draws that view of
the file. The file stays the file; the view is a view of it. The format
itself is `docs/specs/bi-format.md`.

## Status

**Built.** Both modes, the form-style data view with the schema's layout,
validation, refs across the project, and the `:bi` commands — `set`,
`new`, `delete`, `add`, `rename`, `remap`, `prune`, `init`, `schema`,
`migrate`.

## What it looks like

A `.bidata` opens as a form per instance; a `.bischema` as a tree of
types. One pane, in the window the file opened in, with the file's status
row. The data view:

```
▾ Weapon rusty_sword ────────────────────────────────
    name        "Rusty Sword"
    damage      ━●━━━━━━━━━━━━━━  12
    rarity      ‹ common ›                 ← dimmed: the default
  ▸ offset      { x 0.5, y 0 }
  ▸ tags        [melee, starter]
      [ ] two_handed
▸ Weapon dagger ─────────────────────────────────────
▾ Enemy goblin ──────────────────────────────────────
    name        "Goblin"
  ▾ Stats                                  ← a group the schema declared
      hp        40
      speed     ━━●━━━━━━━━━━━━━  1.4
    weapon      ‹ rusty_sword → Weapon ›
  ▾ drops       [rusty_sword, dagger]
      [0]       ‹ rusty_sword → Weapon ›
      [1]       ‹ dagger → Weapon ›
    leader      ‹ goblin_chief → Enemy ›  ⚠ no Enemy goblin_chief
    tint        ██ #c83c1e
    falloff     ▁▂▃▅▆▇██▇▆▅▃▂▁▁▁  5 points
    ramp        ████████████████  3 stops
    damage      99                        ⚠ unknown key
```

A number with both `min` and `max` is a slider; a bool a checkbox; an
enum, a ref or an optional a choice turned with `h` and `l`; a string its
text; a struct or a list a fold; an `rgb` or `rgba` a brick painted its
colour beside the hex; a `curve` a sparkline of its shape beside its point
count; a `gradient` sixteen bricks sampled across it beside its stop
count. A set value is drawn plainly, an inherited
one dim, a read-only one muted; a warning rides on its row after `⚠`. A
row's `doc` — the schema's — shows in the status row while it is selected.

The schema view is a tree, one row per type, its fields under it, each
field's attributes under that:

```
▾ Rarity  enum        Drop tier, drives colour and loot tables
    common
    rare
    epic
▾ Weapon  struct
  ▸ name        string
  ▾ damage      i32 = 10  0..999  @Stats
      type      i32
      default   10
      min       0
      max       999
      step      —
      doc       —
      group     Stats
      order     —
      label     —
      readonly  —
      show_if   —
      hide_if   —
      widget    —
```

## Opening

The extension is the trigger and `$dialect` the confirmation:

- `bi level1.bidata`, `:e level1.bidata`, `Enter` on it in the tree, the
  file picker — every path a file opens by — reads the extension, opens
  the text buffer as ever, and puts the property view over it, the first
  instance open. A file that does not read opens as text with the error
  in the status, as the format says.
- `:set editor bischema` and `:set editor bidata` on a text window ask
  for the view by hand. An **empty buffer** gets the skeleton written into
  it first — `$dialect`, `types: {}` or `$schema` and `instances: []` —
  with `$schema` filled in when exactly one `.bischema` sits beside the
  file. A **broken buffer** keeps the view up with the error as its one
  row, so that `:bi init`, `:bi schema <path>` and `:bi migrate` can mend
  it; `:set editor text` puts the text back. `:set editor` alone says
  which is up.
- A `.bidata` names its schema by `$schema`, relative to itself; a schema
  that is open in a buffer is read from the buffer, not the disk, so an
  unsaved change to a field's default already shows in the data.

## The keys

The pane borrows the tree's keymap for what it does not claim, so `Ctrl-W`
and the leader still work, and reads its own vocabulary on top. `h` and
`l` are the form's keys on a value and the tree's keys on anything else:

```
j  k  ↓ ↑   Ctrl-D  Ctrl-U   pick a row; counts multiply      gg  G   first, last
Tab  Shift-Tab               the next, previous instance or type
l  →         on a value: turn it up — a number by its step, an enum or a ref
             to the next, a bool flipped, an optional on; counts multiply
             on a fold, a group, an instance, a type: open it
h  ←         on a value: turn it down; otherwise close it, or go to the parent
H  L         ten steps
Backspace    the parent row
Enter  i     on a bool: flip it — a form's Enter on a check
             on any other value: edit it on the ex line — `:bi set goblin.hp 40`
             on a colour, a curve, a gradient: its editor over it — see below
             otherwise open or close the row
y            yank the value, with its type; a whole instance from its header
p            put it on this row — one of the same type, in this file or another
Space        flip a bool, cycle an enum or a ref, an optional between — and
             the inner value's default; a number one step up
Ctrl-A  Ctrl-X   the same as l and h on a value, for the vim hand
J  K         move the row among its siblings: an instance, a list item, a
             type, a field, an enum value — display order is the file's order
a            add: an item to a list, an instance to a data file, a field to a
             struct, a value to an enum, a type to a schema — the ones that
             need a name prefill the ex line
dd           remove: a set value goes back to its default, a list item goes,
             an unknown key goes, an instance goes — one other instances
             reference prefills `:bi delete Enemy goblin` instead, and that
             line is the confirmation — a field, a value or an unused type
             leaves the schema; an attribute goes back to unset
gd           on a ref: jump to its target, in this file or another
u  Ctrl-R    undo, redo — the buffer's; the view follows
:            the ex line
```

**`y` and `p` carry a typed value**, not text. `y` on a row keeps the
value and its type in the session — `40` as an `i32`, a struct as itself,
a whole instance's fields from its header row — and puts the value's
spelling on the register ring too, so `p` in a text window pastes what
`Enter` would have shown. `p` on a row of the same type writes the value
there, in this file or another data file of the schema, as one edit; an
optional of the type takes it as well. Any other row says `yanked i32,
this is string` and writes nothing, and a read-only row refuses as `Space`
would. `p` on an instance header of the yanked instance's type replaces
its fields, keeping its id. `y` on a schema row, a group or an error row
has nothing typed to take and says so.

Every key that changes something is one edit of the buffer and one undo
step of it; the view re-reads the buffer after, so there is one direction
of data flow and no second history. A read-only field (the schema's
`readonly`) answers every one of these with `read-only`.

## Colours, curves and gradients

An `rgb` or `rgba` row draws a brick — two cells painted the colour, an
`rgba` blended over the pane's background by its alpha — and the hex after
it. A `curve` row draws a sparkline, the curve sampled across sixteen
cells with `y` clamped to `0..1`, and says how many points it has. A
`gradient` row draws sixteen bricks sampled across the ramp and says how
many stops. None of the three turns: `dd` puts the default back, and
`Enter` opens the value's editor.

`Enter` on a colour opens the colour picker (`color-picker.md`), on a
curve the curve editor (`curve.md`), on a gradient the gradient editor
(`gradient.md`), each over that value: the picture splits to the right
of the view, the form down the edge, focus on the picture, and the keys
there change the value by rewriting the JSON in this buffer. The view
re-reads after every one, so the brick, the sparkline or the bricks
follow. An inherited value is written into the instance first, as its own
undo step, so there is a literal to edit; a read-only one refuses with
`read-only`. `:set editor color`, `curve` and `gradient` on the view do
the same as `Enter`. `Esc` on the picture comes back to the view with the
tool open, `:q` closes it. When an edit made in the view moves the
literal, the tool finds it again by its path rather than by its bytes, so
editing `goblin.hp` above the curve does not lose the plot.

## Layout

The schema says how a struct's fields are laid out, Odin's way: as
attributes on the field. Every one is optional and every reader that does
not know them ignores them, which is what keeps them inside bi/1.

| Attribute  | Value                    | Effect in the data view                                    |
|------------|--------------------------|------------------------------------------------------------|
| `group`    | `"Stats"`, `"Stats/Combat"` | The field sits under a titled, foldable section; `/` nests |
| `order`    | number                   | Fields sort by it, lowest first; unset is 0; the array order breaks ties |
| `label`    | string                   | The row says this instead of the field's name              |
| `readonly` | `true`                   | Shown, never turned, set, added to or removed              |
| `show_if`  | condition                | The row is there only while the condition holds            |
| `hide_if`  | condition                | The row is gone while the condition holds                  |
| `widget`   | `"toggle"`, `"inline"`   | `toggle`: an enum as every value with the current one marked; `inline`: a struct always open, with no fold |

A **condition** names a sibling field: `two_handed`, `!two_handed`,
`rarity == epic`, `rarity != epic`, `hp > 10`, `hp >= 10`, `hp < 10`,
`hp <= 10`. The value is `true`, `false`, a number, or a string with or
without quotes; the comparisons want a number. A condition that names no
sibling is a schema error. Conditions read the resolved values, so a
default counts.

A struct may carry a `groups` object beside `fields` with options per
group: `{ "Stats": { "collapsed": true, "doc": "the numbers" } }`. A
collapsed group starts closed; `doc` is the group row's status text.

A number with both `min` and `max` draws as a slider without asking. The
attributes are rows of the schema view like any other: `Enter` edits one,
`Space` and `l` turn `readonly`, `widget` and `type` through their
choices, `dd` unsets one.

## Paths

The ex forms name a row by a path, the same string `Enter` prefills.

In a data file: `<id>.<field>`, deeper with `.` for a struct's field and
`[n]` for a list item — `goblin.hp`, `dagger.offset.x`, `goblin.drops[1]`.
Groups are layout, not data, and do not appear in paths. Ids are per
type, so an id two types share is written `Enemy:goblin`. The instance's
own id is `goblin.$id`; setting it rewrites every ref to it in every data
file of the schema, which is what the format promises.

In a schema: `<Type>.<field>.<attr>` with `type`, `default`, `min`, `max`,
`step`, `doc`, `group`, `order`, `label`, `readonly`, `show_if`, `hide_if`
and `widget` as the attributes, `<Type>.<field>.name` to rename the
field, `<Type>.doc` for the type's own line, `<Enum>.values[n]` for a
value. Renaming a field or an enum value through `set` is the
refactoring: every data file of the schema is rewritten in the same step.
Changing a field's `type` drops a default, a range or a widget the new
type cannot carry, rather than refusing.

## Values on the ex line

`:bi set <path> <value>` parses `<value>` by the field's type: `true` and
`false`; a number, whole and within its width for the integer types; a
string as typed, quotes optional and JSON escapes honoured inside them; an
enum by name; a ref by id; `null`, `none` or `-` for an absent optional;
a colour as hex with or without the `#`, six digits or eight; a list, a
struct, a curve or a gradient as JSON. A value that does not parse is refused naming
what the field takes; a number outside `min..max` is written and warned
about, as the format says. `:bi set <path>` with no value reports it, and
`Enter` on an unset attribute or an absent optional prefills nothing, so
what is typed is the value.

Writing a value equal to the default removes the key: the row goes dim
and the file loses a line, which is what sparse storage means. A list or
struct value is materialised whole from the resolved value before an item
inside it is changed, so `goblin.drops[1]` on an inherited list writes the
list, and `rusty_sword.offset.y` on an inherited struct writes the whole
struct with `x` still at the field's default.

## `:bi`

```
:bi set <path> [value]           the value, or a report
:bi new <Type> <id>              a new instance at the end of a data file, required refs blank
:bi delete <Type> <id>           an instance, however many refs it has; they dangle and warn
:bi add <Name> struct [f:t …]    a struct, with fields — `:bi add Color struct r:u8 g:u8 b:u8`
:bi add <Name> enum [v …]        an enum with its values — `:bi add Rarity enum common rare epic`
:bi add <Type> <f>:<t> …         fields on a struct; `:bi add Weapon speed f32` for one
:bi add <Enum> <v> …             values on an enum
:bi rename <Type>.<old> <new>    a field, an enum value (schema) or an id (data), data files rewritten
:bi rename <Type> <New>          a type: its key, every type expression, every instance's $type
:bi remap <Type>.<old> <new>     an unknown key renamed across the data files — a rename made by hand
:bi prune                        every unknown key of this file removed
:bi init                         the skeleton written over the buffer, $schema guessed
:bi sample                       the sample schema or data written over the buffer — see below
:bi schema [path]                a data file's $schema set, or reported
:bi migrate                      an older `$dialect` brought up to bi/1
```

Each refuses with a message when the path does not exist or the name is
taken; a thing just added is opened and selected. `:bi` alone lists them.
`init`, `sample`, `schema` and `migrate` work on a broken file, since they
are how it gets mended; the rest want a file that reads.

**The sample.** `examples/props/game.bischema` and `level1.bidata` are a
pair that uses every type expression, every layout attribute, sparse
storage, refs and an optional — the thing to open first. `bi gen sample`
on the command line writes both into the working directory (`bi gen
sample schema` or `data` for one), leaving a file that already exists
alone and printing the text instead; `:bi sample` writes the one of the
buffer's kind into the buffer, the data's `$schema` pointed at whatever
`.bischema` sits beside it.

**The project's data files** are every `.bidata` under the project root
(the tree's root, else the schema's directory) whose `$schema` resolves to
the schema in hand, ignoring what git ignores. The index over them is
what checks duplicate ids and dangling refs, what `l` cycles a ref
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
pub enum IntKind { I8, I16, I32, I64, U8, U16, U32, U64 }
pub enum TypeExpr { Bool, Int(IntKind), F32, F64, Str, Named(String), List(Box<..>), Optional(Box<..>), Ref(String) }
pub struct Cond { field, op, value }                   // show_if / hide_if
pub enum Widget { Toggle, Inline }
pub struct FieldDef { name, ty, default, min, max, step, doc,
                      group, order, label, readonly, show_if, hide_if, widget }
pub enum TypeDef { Struct { fields, doc, groups }, Enum { values, doc } }
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
pub struct Props { kind, buffer, path, schema_path, raw: Value, schema, data,
                   diagnostics, index, rows: Vec<Row>, selected, expanded, collapsed, error, seen }
pub enum RowWidget { Plain, Header, Group, Fold, Slider(f32), Check(bool), Choice, Toggle, Text,
                     Color([u8; 4]), Curve([u8; 16]), Gradient([[u8; 4]; 16]) }
                     // the brick's rgba; sixteen samples, 0..=7; sixteen sampled colours
pub struct Row { pub key, pub depth, pub label, pub value, pub kind: RowKind, pub widget,
                 pub inherited, pub readonly, pub turnable, pub warning, pub doc,
                 pub expandable, pub expanded }
pub enum Edit { Text(String), Prompt(String), Refactor { text, refactor } }
pub enum ToolKind { Color, Curve, Gradient }
impl Props {
    pub fn tool_target(&self, key: &str) -> Result<Option<(ToolKind, String, Option<Edit>)>, String>;
                                    // a colour, curve or gradient row: its path, and the edit that materialises it
    pub fn locate(&self, text: &str, path: &str) -> Option<(usize, usize)>;   // the value's bytes in the text
}

// locate.rs
pub fn value_span(text: &str, index: usize, segs: &[Seg]) -> Option<(usize, usize)>;
```

`locate` is a small JSON walker over the buffer's text: down `instances`
to the instance, then key by key and index by index to the value, giving
the byte span of the value as written, whatever the layout. It is what
opens the curve tool on the right bracket and what finds the literal again
after the view rewrites the file.

The raw `Value` — `serde_json` with `preserve_order` — is the document;
the parsed `Schema` and `DataFile` are read from it for validation and
row building, and every edit is a change to the raw value followed by
`write_*`, so unknown keys on definitions and instances survive without
anyone listing them. An edit returns the new text; the editor puts it in
the buffer as one undo step and the view re-reads on the next sync, the
curve tool's arrangement.

Rows are rebuilt from the document, the layout and the expanded set after
every change; selection and expansion are held by row key — `inst:3/hp`,
`inst:0/@Stats`, `type:Weapon/field:damage/default` — so an edit that
reorders nothing keeps the cursor where it was, and one that removes the
selected row lands on the row that took its place. A row says which
widget it draws as; the core decides, the frontend draws.

## The view in the editor

`Content::Props(Box<Props>)` on a window, for the tree's reason: the
selected row and what is open are view state. `Content::buffer` reports
the buffer, so `:w`, `:q`, `:bd`, the modified marker and the buffer list
see the file as the file it is. The pane's key grammar is `PropsCmd`,
read in `input.rs` beside the form's; the after-key sync re-reads any
props window whose buffer's edit counter moved.

## Tests

- `TypeExpr::parse` reads every primitive including the eight integer
  widths, a name, and nested generics; refuses whitespace, an unknown
  generic, `ref<i32>`, `optional<optional<T>>`. A value outside its
  width does not encode.
- `Schema::parse` on the full example finds four types; every schema
  error in the format's table is reported with its type or field named;
  every layout attribute parses and a condition naming no sibling, a
  widget on the wrong type, an empty group are refused.
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
- `rgb`, `rgba` and `curve` parse as types; a colour reads six or eight
  hex digits with or without the `#` and refuses `red`; an `rgba` given
  six digits gets `ff`; a curve refuses a point with `x` outside `0..1`,
  out of order, or a non-number; the defaults are black, opaque black and
  the linear curve.
- `value_span` finds a field, a nested field and a list item in a
  hand-laid-out file and in the writer's layout.
- Rows: the example expands to the tree above with the right widgets; a
  colour row is `Color` with its bytes, a curve row `Curve` with sixteen
  samples of the shape, neither turnable;
  inherited rows are dimmed; a dangling ref carries its warning; the
  `expanded` set survives a rebuild; groups nest and start collapsed when
  told; `order` sorts; `show_if` and `hide_if` follow the values; `label`
  and `readonly` show; `toggle` and `inline` draw as they say.
- Edits: `set goblin.hp 41` rewrites one line; `set rusty_sword.rarity
  common` removes the key; `set goblin.drops[1] warhammer` writes the
  list; `set rusty_sword.$id iron_sword` rewrites three places; `Space`
  on a bool, an enum, a ref, an optional; `l` clamps at `max` and an
  integer at its `min`; `dd` on a set value, a list item, an unknown key,
  an instance; `a` on a list; `J`/`K` move an instance, an item, a
  field, a value, a type; `new`, `delete`, `add` with many values and
  fields, `rename` of a field, a value, an id and a type, `remap`,
  `prune` each as one undo step; a read-only field refuses; a bad value
  is refused naming the type.
- Enter on a curve row opens the plot to the right and the form at the
  edge with the props window as the source; `j` on the plot rewrites one
  number in the JSON and the view's sparkline follows; an inherited curve
  is written into the instance first; a read-only one refuses; editing
  another field in the view keeps the plot on its curve; `:q` on the plot
  focuses the view again.
- Opening: a `.bidata` opens in the view with the first instance open; a
  missing `$dialect` opens as text with the error; `:set editor bidata`
  on an empty buffer writes the skeleton, on a broken one keeps the view
  with the error and `:bi init` mends it; `:set editor text` and back; a
  schema edited by hand in a split updates the data view; `:w` normalises
  and writes; `u` restores the text and the view.
