# Forms

A tool with knobs — the normal map generator's strength, blur and filter,
a curve's evaluator — needs somewhere to turn them, and the ex line is the
wrong place for a value you adjust while watching a picture. A form is a
pane of fields driven by the keys you already use for lists: `j` and `k`
to pick one, `h` and `l` to turn it. One content kind, built once, that
any tool fills.

## Status

**Built.**

## What a form is

```rust
pub struct Form {
    tool: String,            // the `:tool <name>` that owns it
    title: String,
    fields: Vec<Field>,
    selected: usize,
    undo: Vec<(usize, Value)>, redo: Vec<(usize, Value)>,
    generation: u64,         // bumped by every change; the owner watches it
}

pub struct Field { name: String, label: String, kind: Kind, value: Value }

pub enum Kind {
    Float { min: f32, max: f32, step: f32 },
    Int   { min: i64, max: i64, step: i64 },
    Bool,
    Choice { options: Vec<String> },
}
pub enum Value { Float(f32), Int(i64), Bool(bool), Choice(usize) }
```

Core state with no terminal in it, like a tree or a results list: the
frontend draws fields however it draws, and a GUI would make real widgets
of the same list. It is a `Content::Form` on a window, for the reason the
tree is: the selected field is view state, and the form is a pane you can
split beside, resize and close.

**Fields carry their own range and step**, so strength moves by tenths
while blur moves by whole pixels, and a slider can be drawn from the
range. A value is clamped to its range whichever way it is set. A bool
toggles; a choice cycles through its options and wraps.

**The owner reads, the form does not call back.** Every change bumps
`generation`; after each key the editor asks every tool whether its form
moved and recomputes what depends on it. No closures in the form, which
is what keeps it plain data a frontend can hold.

## The keys

A form window dispatches like the debug panes: its own vocabulary, with
`[keys.tree]` borrowed for what it does not claim, so `Ctrl-W` and the
leader keep working.

```
j  k   Down  Up     pick a field; counts multiply     gg  G   first, last
h  l   Left  Right  turn it one step; counts multiply
H  L                ten steps
Space               toggle a bool, cycle a choice
Enter  i            edit the value on the ex line: `:tool <owner> <field> <value>`
u  Ctrl-R           undo, redo a change
Tab                 cycle the field named `map`, when the form has one — see below
Esc                 back to the picture the form belongs to
```

`Enter` prefills the ex line rather than opening a prompt of its own: the
same `:tool <owner> <field> <value>` works typed cold, so everything a
form can do is scriptable, and a bare `:tool <owner> <field>` reports the
value. A number that does not parse, a choice that is not an option, a
bool that is not `true` or `false` — each is refused with a message
naming what the field takes.

**`Tab`** is a convenience for tools with several outputs: a field named
`map` is the one that says which output the picture shows, and `Tab` from
anywhere in the form cycles it without first selecting it. A form with no
such field ignores `Tab`.

## The status row

`Normal map` on the left — the form's title — and `7 fields` where a text
pane says `12:40`. No mode segment: like an image, a form has no modes.

## Tests

- `nudge` moves a float by its step and clamps at both ends; a count
  multiplies; an int the same by whole steps.
- `toggle` flips a bool and cycles a choice with wrap-around.
- `set` by name parses per kind and refuses with a message naming the
  field's options or range.
- `undo` restores the previous value and `redo` reapplies; a new change
  clears redo; each bumps the generation.
- The keys: `j`/`k`/`gg`/`G` select, `h`/`l`/`H`/`L` nudge with counts,
  `Space` toggles, `Enter` prefills the ex line, `u` undoes, `Tab` cycles
  `map`, `Esc` leaves.
- The renderer draws a slider whose filled length follows the value and
  highlights the selected field.
