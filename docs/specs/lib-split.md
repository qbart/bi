# Library split

bee is one binary crate. Nothing can link its core, so a second frontend — a
GUI, an embedding, a headless test harness — has no way in, and the only way to
drive the editor is to press keys at a terminal.

The fix is small because the core is already clean. Of nine modules, exactly two
name a terminal library: `ui.rs` renders with ratatui, and `input.rs` takes a
`crossterm::KeyEvent`. `buffer`, `editor`, `history`, `motion`, `picker`,
`registers` and `syntax` contain zero terminal references, and every item in the
crate is already `pub` — there is not one `pub(crate)` to widen.

## Status

**Built.**

Scope is the boundary and nothing else. Behaviour is preserved exactly,
including the quirks noted below. The other architectural items in the README —
the config language, the cursor on `Buffer`, `Editor::scroll` as a row index —
are untouched. Dragging any of them in would bury the change that matters.

## Shape

Cargo infers both targets from `src/lib.rs` and `src/main.rs`, so no `[lib]` or
`[[bin]]` stanza is needed. Binary-only code moves under `src/tui/` so the
boundary is legible in the file tree, rather than nine library files and three
binary files sitting side by side with nothing to tell them apart.

```
src/lib.rs        pub mod buffer, editor, history, input, key,
                  motion, picker, registers, syntax
src/key.rs        new — KeyCode, Mods, Key
src/input.rs      one import swapped: crossterm → crate::key
src/{buffer,editor,history,motion,picker,registers,syntax}.rs
                  unchanged

src/main.rs       mod tui; + terminal setup, restore, event loop
src/tui.rs        module root — pub mod keys, render
src/tui/render.rs was ui.rs — verbatim except crate:: → bee::
src/tui/keys.rs   new — crossterm KeyEvent → bee::Key
```

`main.rs` calls `tui::render::render(frame, ed, pending)` and
`tui::keys::translate(ev)`. No re-exports: two callers do not justify a facade,
and the paths say where the code lives.

`ui.rs` lifts out cleanly because it is a leaf: it imports `editor`, `picker`
and `syntax`, and nothing imports it.

`ratatui` stays a plain dependency — Cargo has no per-target dependencies for a
lib and bin in the same package. The boundary is therefore not a compiler rule:
**no module reachable from `lib.rs` may name `ratatui` or `crossterm`.** Only
splitting into a workspace would let the compiler enforce that, and a workspace
is not worth its churn for one frontend.

It is enforced by a test instead — see *Tests* below. A grep in the README would
have been the cheaper option and the wrong one: nobody runs it, and it cannot
tell a doc comment that *names* the boundary from code that *breaks* it.

## Keys

`input.rs` is editor semantics — the `[count] operator [count] motion` state
machine — so it belongs in the library. What does not belong is crossterm's
vocabulary of key events.

The surface actually used is small. `input.rs` reads `Char`, `Esc`, `Enter`,
`Backspace`, `Tab`, the four arrows, `Home`, `End`, and the `CONTROL` modifier.
That is the whole list.

```rust
pub enum KeyCode {
    Char(char),
    Esc, Enter, Backspace, Tab,
    Left, Right, Up, Down, Home, End,
}

pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

pub struct Key {
    pub code: KeyCode,
    pub mods: Mods,
}
```

`Mods` carries `alt` and `shift` even though only `ctrl` is read today. This is
the type a config-driven keymap will have to parse into, and widening it later
means revisiting every match arm in `input.rs`. `alt` in particular is what the
next real keymap wants (`<A-j>`); paying thirty lines now is cheaper than the
retrofit.

Constructors keep the tests terse — they are what most of the churn in
`input.rs`'s test module turns into:

```rust
impl Key {
    pub fn char(c: char) -> Self;      // Char(c), no modifiers
    pub fn ctrl(c: char) -> Self;      // Char(c), ctrl
    pub fn code(code: KeyCode) -> Self; // no modifiers
}
```

### Two rules that prevent silent breakage

**Match on `key.code`; consult `key.mods` only in guards.**

```rust
match key.code {
    KeyCode::Char('r') if key.mods.ctrl => self.plain(Action::Redo),
    KeyCode::Char(c) => self.normal_char(c),
    KeyCode::Esc => { /* … */ }
    // …
}
```

Today `KeyCode::Char(c)` matches whatever the modifiers are, because `input.rs`
never looks at anything but `CONTROL`. Now that `alt` and `shift` are populated,
matching on a whole `Key` value would start excluding keys that currently work —
`D` arrives as `Char('D')` with `SHIFT` set, and would stop matching. Matching on
`code` alone preserves the existing behaviour by construction.

This also preserves an existing quirk: `Alt+d` in normal mode acts as plain `d`,
because nothing consults `alt`. That is true today and stays true. A keymap that
wants to distinguish them is a config-language problem, not this change.

**Translation returns `Option<Key>`, and drops what it cannot map.**

```rust
// src/tui/keys.rs
pub fn translate(ev: KeyEvent) -> Option<Key> {
    let code = match ev.code {
        CtCode::Char(c) => KeyCode::Char(c),
        CtCode::Esc => KeyCode::Esc,
        // … the ten codes above …
        _ => return None,
    };
    Some(Key {
        code,
        mods: Mods {
            ctrl: ev.modifiers.contains(KeyModifiers::CONTROL),
            alt: ev.modifiers.contains(KeyModifiers::ALT),
            shift: ev.modifiers.contains(KeyModifiers::SHIFT),
        },
    })
}
```

F-keys, `PageUp`, `Delete`, `Insert` and the media keys map to nothing, so they
are dropped at the frontend. That is exactly what happens today, one layer
lower: they reach `input.rs` and fall through its `_ => None` arm. Dropping them
in `translate` moves the fall-through without changing the outcome.

The event loop in `main.rs` becomes:

```rust
Event::Key(key) if key.kind == KeyEventKind::Press => {
    if let Some(key) = tui::keys::translate(key)
        && let Some(cmd) = input.on_key(key, &ed.mode)
    {
        ed.status.clear();
        ed.apply(cmd);
    }
    ed.sync_syntax();
}
```

## Tests

`input.rs`'s existing tests construct `KeyEvent::new(KeyCode::Char('d'),
KeyModifiers::NONE)`. They become `Key::char('d')`, `Key::ctrl('r')` and
`Key::code(KeyCode::Esc)`. Mechanical, and the test module gets shorter.

`src/tui/keys.rs` gets its own tests: that `CONTROL` maps to `mods.ctrl`, and
that an unmapped code yields `None`.

`tests/lib_boundary.rs` is new and is the point of the exercise. It plays the
part of an embedder: it links `bee`, never names a terminal, and drives a
headless session through the public API — type, move, delete, undo, redo, yank,
paste, switch modes. It can only compile if the core is genuinely frontend-free,
so the split proves itself on every `cargo test`.

Two guards in the same file cover what compiling cannot:

`no_library_module_names_a_terminal_crate` reads each module `lib.rs` declares
and fails on any non-comment line naming `ratatui` or `crossterm`. Comment lines
are skipped deliberately — the doc comments explaining the boundary name the
crates they forbid, and a check that fails on its own documentation gets deleted
rather than fixed.

`the_module_list_matches_what_lib_rs_declares` keeps that check honest. The
module list is a literal, not a directory walk, so that adding a module is a
deliberate decision about which side of the boundary it lands on — and this test
fails if `lib.rs` gains a `pub mod` the list does not cover, which is otherwise a
silent hole.

Both were verified by injecting a violation and watching them fail, not by
watching them pass.

## Verification

- `cargo build` clean; `cargo test` green at 148.
- `cargo clippy --all-targets` surfaces one pre-existing `collapsible_if` in
  `editor.rs:312`, untouched by this change and left alone.

## Docs

The README's "No `[lib]` target, and `input.rs` speaks crossterm's key types"
bullet leaves *Architectural, and cheaper to fix now than later*. It is replaced
by a note that the boundary exists and is test-enforced rather than
compiler-enforced — the honest version, since a reader who assumes the compiler
is guarding it will eventually be surprised by what a workspace would have
caught and this does not.

`main.rs`'s module doc currently describes the whole editor. It becomes a
description of the terminal frontend, and the architectural note moves to
`lib.rs`.
