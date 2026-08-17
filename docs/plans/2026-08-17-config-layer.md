# Config Layer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give bi a real config file — `~/.config/bi/config.toml` — that sets
options, reloads at runtime with `:reload`, and is created and edited through
`bi config init` / `bi config edit`.

**Architecture:** The library owns the config types and the TOML parser
(`bi::config`); the frontend owns only where the file lives, supplied through a
`ConfigSource` trait so the core never learns what a filesystem is. Options move
off `Session`'s loose fields into one `Options` struct that is simultaneously
the `[options]` table and the `:set` namespace, so both reach one place.
Diagnostics are non-fatal: a bad line is dropped and reported, never a refusal
to start.

**Tech Stack:** Rust 2024, `toml_edit` (spans, for line-numbered diagnostics),
`anyhow`, existing `ratatui` frontend.

**Spec:** `docs/specs/config.md`

## Global Constraints

- This plan is **step 1 of 3** from the spec's "Order" section: the config layer
  end to end, with `[options]` as its only content. The theme (step 2) and the
  keymap (step 3) get their own plans. Do not implement either here.
- Everything in `src/config/` is library code. It must never name a terminal
  crate — `tests/lib_boundary.rs` proves this by linking `bi` and compiling.
  `toml_edit` is not a terminal crate and is fine.
- `rustfmt.toml` sets `use_small_heuristics = "Max"` and `max_width = 100`.
  Run `cargo fmt` before every commit. Comments are wrapped by hand near 80.
- Tests live in the inline `#[cfg(test)] mod tests` of the file they cover, the
  existing convention (`src/editor.rs:3590`). Helpers `ex(&mut ed, "…")` and
  `ScratchDir` already exist there.
- Option names in `[options]` are spelled exactly as `:set` spells them.
  Underscores, never hyphens.
- Commit after every task. Commit messages follow the repo's style: a
  lowercase `area: what changed` subject, then prose explaining *why*.
- Never create a branch — commit straight to `master`.

---

### Task 1: `toml_edit`, `Diagnostic`, and byte offset → line

**Files:**
- Modify: `Cargo.toml`
- Create: `src/config/mod.rs`
- Modify: `src/lib.rs:19-30` (add `pub mod config;`)

**Interfaces:**
- Consumes: nothing
- Produces: `bi::config::Diagnostic { line: usize, message: String }`;
  `bi::config::line_of(src: &str, offset: usize) -> usize` (crate-visible)

- [ ] **Step 1: Add the dependency**

```bash
cargo add toml_edit
```

Then move the new line under the existing dependencies in `Cargo.toml` so the
list stays grouped, and add a comment above it:

```toml
# `toml_edit` rather than `toml`: it keeps byte spans after parsing, which is
# the difference between "unknown option" and "config.toml:7: unknown option".
toml_edit = "0.23"
```

Use whatever version `cargo add` actually resolved — do not hand-edit the
version number.

- [ ] **Step 2: Write the failing test**

Create `src/config/mod.rs`:

```rust
//! bi's config: the types, the parser, and the source a frontend supplies.
//!
//! The library owns the types and the parser because a keymap is editor
//! semantics — the same argument `key.rs` makes for `Key`. A frontend owns
//! only where the file lives. See `docs/specs/config.md`.

/// A problem with a config file. Reported, never fatal: an editor you cannot
/// launch because of a typo in its config is an editor you cannot use to fix
/// the typo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// 1-based, into whichever file it came from.
    pub line: usize,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_of_counts_newlines_before_the_offset() {
        let src = "one\ntwo\nthree\n";
        assert_eq!(line_of(src, 0), 1);
        assert_eq!(line_of(src, 3), 1, "the newline itself still ends line 1");
        assert_eq!(line_of(src, 4), 2);
        assert_eq!(line_of(src, 8), 3);
        assert_eq!(line_of(src, 999), 3, "past the end clamps rather than panics");
    }

    /// Pins the dependency's span behaviour, because every diagnostic line
    /// number in this module is built on it. If `toml_edit` renames this API,
    /// this test says so in thirty seconds instead of leaving every
    /// diagnostic silently pointing at line 1.
    #[test]
    fn toml_edit_reports_key_spans() {
        let src = "[options]\nnumber = 5\n";
        let doc: toml_edit::ImDocument<&str> = toml_edit::ImDocument::parse(src).unwrap();
        let table = doc["options"].as_table().unwrap();
        let (key, _) = table.get_key_value("number").unwrap();
        let span = key.span().expect("keys carry spans after a fresh parse");
        assert_eq!(line_of(src, span.start), 2);
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

```bash
cargo test --lib config::
```

Expected: FAIL — `cannot find function line_of in this scope`.

- [ ] **Step 4: Implement `line_of`**

Add to `src/config/mod.rs`, above the test module:

```rust
/// The 1-based line a byte offset falls on.
///
/// `toml_edit` reports spans as byte ranges; a diagnostic wants a line. Out of
/// range clamps to the last line rather than panicking, because a span that
/// disagrees with its source is a dependency bug and should not take the
/// editor with it.
pub(crate) fn line_of(src: &str, offset: usize) -> usize {
    let offset = offset.min(src.len());
    1 + src[..offset].bytes().filter(|&b| b == b'\n').count()
}
```

- [ ] **Step 5: Declare the module**

In `src/lib.rs`, add `pub mod config;` to the module list, keeping it
alphabetical — between `buffer` and `editor`:

```rust
pub mod buffer;
pub mod config;
pub mod editor;
```

- [ ] **Step 6: Run the tests to verify they pass**

```bash
cargo test --lib config::
```

Expected: PASS, 2 tests.

If `toml_edit_reports_key_spans` fails to compile, the installed `toml_edit`
names this API differently. Check `cargo doc -p toml_edit --open` for the
immutable-document type and its `span` accessors, adapt this one test, and
carry the corrected spelling through the rest of the plan. Everything else here
depends only on "a key has a byte offset".

- [ ] **Step 7: Commit**

```bash
cargo fmt
git add Cargo.toml Cargo.lock src/config/mod.rs src/lib.rs
git commit -m "config: the module, and line numbers for its diagnostics

A config error that cannot say which line it is on is a config error the
user has to bisect by hand. toml_edit keeps byte spans where toml does
not, so the dependency is chosen for its diagnostics rather than its
parsing."
```

---

### Task 2: `Options` and the compiled-in defaults

**Files:**
- Modify: `src/config/mod.rs`
- Create: `src/config/default.toml`

**Interfaces:**
- Consumes: `Diagnostic` from Task 1
- Produces: `Options { number: LineNumbers, hlsearch: bool }` with `Default`;
  `Config { options: Options }` with `Default`;
  `pub const DEFAULT_TOML: &str`;
  `OptionValue { Int(i64), Bool(bool) }`;
  `Options::set(&mut self, name: &str, value: OptionValue) -> Result<(), String>`;
  `Options::get(&self, name: &str) -> Option<OptionValue>`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `src/config/mod.rs`:

```rust
#[test]
fn shipped_defaults_agree_with_the_rust_fallback() {
    // `default.toml` is the documented source of the defaults and
    // `Options::default()` is what an embedder with no config gets. They
    // must say the same thing, and nothing but this test keeps them honest.
    assert_eq!(Config::default().options, Options::default());
}

#[test]
fn set_and_get_round_trip_every_option() {
    let mut options = Options::default();

    assert_eq!(options.set("number", OptionValue::Int(5)), Ok(()));
    assert_eq!(options.number, LineNumbers::Every(5));
    assert_eq!(options.get("number"), Some(OptionValue::Int(5)));

    assert_eq!(options.set("hlsearch", OptionValue::Bool(true)), Ok(()));
    assert!(options.hlsearch);
    assert_eq!(options.get("hlsearch"), Some(OptionValue::Bool(true)));
}

#[test]
fn set_rejects_unknown_names_and_bad_values() {
    let mut options = Options::default();

    assert_eq!(options.set("nmber", OptionValue::Int(5)), Err("unknown option: nmber".into()));
    assert_eq!(options.get("nmber"), None);

    assert_eq!(
        options.set("number", OptionValue::Int(-7)),
        Err("number takes 0 (off), -1 (relative) or a count".into())
    );
    assert_eq!(options.number, LineNumbers::Every(1), "a rejected set changes nothing");

    assert_eq!(
        options.set("hlsearch", OptionValue::Int(1)),
        Err("hlsearch takes true or false".into())
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --lib config::
```

Expected: FAIL — `cannot find type Config`, `cannot find type Options`.

- [ ] **Step 3: Write `default.toml`**

Create `src/config/default.toml`:

```toml
# bi's defaults.
#
# Compiled into the binary. A user's config.toml is a PATCH over this file:
# anything they leave out keeps doing what this says, including settings added
# in later versions. See docs/specs/config.md.

[options]
number = 1
hlsearch = false
```

- [ ] **Step 4: Implement the types**

Add to `src/config/mod.rs`, above the test module:

```rust
use std::sync::OnceLock;

use crate::editor::LineNumbers;

/// bi's defaults, as the file that documents them.
pub const DEFAULT_TOML: &str = include_str!("default.toml");

/// A value an option can hold, in the one shape both `:set` and TOML can
/// produce. `:set` parses a string into one; the parser converts a TOML value
/// into one. Neither needs to know what any particular option is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionValue {
    Int(i64),
    Bool(bool),
}

/// The `:set` namespace. One field per option, spelled as `:set` spells it,
/// because `:set number 5` and `number = 5` are two ways to reach one setting
/// and there is nothing to be gained by giving it two names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Options {
    pub number: LineNumbers,
    /// Off unless asked for: vim does not light the buffer up on a plain `/`.
    pub hlsearch: bool,
}

impl Options {
    /// The single place a name becomes a field. `:set` and the TOML parser
    /// both come through here, so an option cannot exist for one and not the
    /// other.
    pub fn set(&mut self, name: &str, value: OptionValue) -> Result<(), String> {
        match (name, value) {
            ("number", OptionValue::Int(n)) => match LineNumbers::from_setting(n) {
                Some(lines) => self.number = lines,
                None => return Err("number takes 0 (off), -1 (relative) or a count".into()),
            },
            ("number", _) => return Err("number takes 0 (off), -1 (relative) or a count".into()),
            ("hlsearch", OptionValue::Bool(on)) => self.hlsearch = on,
            ("hlsearch", _) => return Err("hlsearch takes true or false".into()),
            _ => return Err(format!("unknown option: {name}")),
        }
        Ok(())
    }

    /// What `:set <option>` reports when given no value.
    pub fn get(&self, name: &str) -> Option<OptionValue> {
        Some(match name {
            "number" => OptionValue::Int(self.number.setting()),
            "hlsearch" => OptionValue::Bool(self.hlsearch),
            _ => return None,
        })
    }
}

/// Everything a config file can say. Options today; a theme and a keymap join
/// them in steps 2 and 3 of `docs/specs/config.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub options: Options,
}

impl Default for Config {
    /// The compiled-in defaults, parsed once.
    ///
    /// Parsed rather than constructed so that `default.toml` is exercised on
    /// every run — if the shipped file stops parsing, no test has to catch it
    /// because nothing starts.
    fn default() -> Self {
        static DEFAULT: OnceLock<Config> = OnceLock::new();
        DEFAULT
            .get_or_init(|| {
                let bare = Config { options: Options::default() };
                parse(DEFAULT_TOML, bare).expect("bi's own default.toml must parse").0
            })
            .clone()
    }
}
```

Add the import of `LineNumbers` to the test module too:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::LineNumbers;
```

- [ ] **Step 5: Run the tests to verify they still fail, now on `parse`**

```bash
cargo test --lib config::
```

Expected: FAIL — `cannot find function parse`. That is Task 3. The three tests
written in Step 1 cannot pass until it exists, which is why Task 3 has no test
of its own for the round-trip.

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add src/config/mod.rs src/config/default.toml
git commit -m "config: options are one struct with one name table

Session held line_numbers and highlight_search as loose fields and
set_option matched on names in a third place, so adding an option meant
touching three. Options::set is now the only place a name becomes a
field, and `:set number 5` and `number = 5` in the file are two spellings
of one call.

Compiles but does not yet build: the parser lands next."
```

Note: this commit does not build on its own. That is deliberate and the message
says so — Tasks 2 and 3 are one change split for reviewability. If you would
rather keep every commit green, do Task 3 before committing either.

---

### Task 3: The parser

**Files:**
- Create: `src/config/parse.rs`
- Modify: `src/config/mod.rs` (add `mod parse; pub use parse::parse;`)

**Interfaces:**
- Consumes: `Config`, `Options`, `OptionValue`, `Diagnostic`, `line_of`
- Produces: `parse(src: &str, base: Config) -> Result<(Config, Vec<Diagnostic>), Diagnostic>`
  — `Err` means the document did not parse at all and nothing can be salvaged;
  `Ok` carries the patched config plus per-item problems.

- [ ] **Step 1: Write the failing tests**

Create `src/config/parse.rs` with the parser's tests first:

```rust
//! TOML into a [`Config`], with a line number on everything that goes wrong.

#[cfg(test)]
mod tests {
    use crate::config::{Config, OptionValue, parse};
    use crate::editor::LineNumbers;

    fn ok(src: &str) -> (Config, Vec<String>) {
        let (config, problems) = parse(src, Config::default()).expect("document parses");
        (config, problems.into_iter().map(|d| format!("{}: {}", d.line, d.message)).collect())
    }

    #[test]
    fn a_user_file_patches_the_defaults() {
        let (config, problems) = ok("[options]\nnumber = 5\n");
        assert_eq!(config.options.number, LineNumbers::Every(5), "overridden");
        assert!(!config.options.hlsearch, "untouched options keep the default");
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn an_empty_file_is_the_defaults() {
        let (config, problems) = ok("");
        assert_eq!(config, Config::default());
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn a_top_level_key_is_not_a_section() {
        let (config, problems) = ok("theme = \"onedark\"\n");
        assert_eq!(problems, ["1: `theme` is not in a section"]);
        assert_eq!(config, Config::default(), "and nothing was applied");
    }

    #[test]
    fn unknown_sections_are_named_with_their_line() {
        let (_, problems) = ok("[options]\nnumber = 5\n\n[nope]\nx = 1\n");
        assert_eq!(problems, ["4: unknown section: nope"]);
    }

    #[test]
    fn unknown_options_are_named_with_their_line() {
        let (config, problems) = ok("[options]\nnumber = 5\nnmber = 9\n");
        assert_eq!(problems, ["3: unknown option: nmber"]);
        assert_eq!(config.options.number, LineNumbers::Every(5), "the good line still applied");
    }

    #[test]
    fn a_bad_value_reports_and_keeps_the_default() {
        let (config, problems) = ok("[options]\nnumber = \"big\"\n");
        assert_eq!(problems, ["2: number takes 0 (off), -1 (relative) or a count"]);
        assert_eq!(config.options.number, LineNumbers::Every(1));
    }

    #[test]
    fn a_value_of_false_is_not_yet_an_unbinding() {
        // `false` unbinds a *key* in step 3. On an option it is just a bool,
        // and `number = false` is a type error rather than a removal.
        let (_, problems) = ok("[options]\nnumber = false\n");
        assert_eq!(problems, ["2: number takes 0 (off), -1 (relative) or a count"]);
    }

    #[test]
    fn malformed_toml_is_the_one_fatal_case() {
        let err = parse("[options\nnumber = 5\n", Config::default())
            .expect_err("an unterminated table header cannot be salvaged");
        assert_eq!(err.line, 1);
        assert!(!err.message.is_empty());
    }

    #[test]
    fn set_and_get_reach_the_same_option() {
        let (config, _) = ok("[options]\nhlsearch = true\n");
        assert_eq!(config.options.get("hlsearch"), Some(OptionValue::Bool(true)));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --lib config::
```

Expected: FAIL — `cannot find function parse`, and `parse.rs` is not yet a
module.

- [ ] **Step 3: Implement the parser**

Add above the test module in `src/config/parse.rs`:

```rust
use toml_edit::{ImDocument, Item, Value};

use super::{Config, Diagnostic, OptionValue, line_of};

/// Parses `src` as a patch over `base`.
///
/// `Err` is the one unsalvageable case: the document is not TOML, so there is
/// nothing to read a single setting out of. Everything else — an unknown
/// section, an unknown option, a value of the wrong type — drops that item,
/// records a [`Diagnostic`], and carries on. A config file is edited by hand
/// and will be wrong sometimes; refusing to start is the wrong answer.
pub fn parse(src: &str, base: Config) -> Result<(Config, Vec<Diagnostic>), Diagnostic> {
    let doc: ImDocument<&str> = ImDocument::parse(src).map_err(|e| Diagnostic {
        line: e.span().map_or(1, |span| line_of(src, span.start)),
        message: e.to_string(),
    })?;

    let mut config = base;
    let mut problems = Vec::new();

    for (key, item) in doc.iter() {
        // `doc.iter()` yields `&str` keys, which carry no span. The key with
        // its span lives on the table.
        let line = doc.get_key_value(key).and_then(|(k, _)| k.span()).map_or(1, |s| line_of(src, s.start));

        let Some(table) = item.as_table() else {
            problems.push(Diagnostic { line, message: format!("`{key}` is not in a section") });
            continue;
        };

        match key {
            "options" => read_options(src, table, &mut config, &mut problems),
            _ => problems.push(Diagnostic { line, message: format!("unknown section: {key}") }),
        }
    }

    Ok((config, problems))
}

fn read_options(
    src: &str,
    table: &toml_edit::Table,
    config: &mut Config,
    problems: &mut Vec<Diagnostic>,
) {
    for (key, item) in table.iter() {
        let line = table
            .get_key_value(key)
            .and_then(|(k, _)| k.span())
            .map_or(1, |s| line_of(src, s.start));

        let Some(value) = option_value(item) else {
            problems.push(Diagnostic { line, message: format!("{key} takes a number or a bool") });
            continue;
        };

        if let Err(message) = config.options.set(key, value) {
            problems.push(Diagnostic { line, message });
        }
    }
}

/// A TOML scalar as an [`OptionValue`]. Anything else — a string, an array, a
/// nested table — is not something an option can hold.
fn option_value(item: &Item) -> Option<OptionValue> {
    match item.as_value()? {
        Value::Integer(n) => Some(OptionValue::Int(*n.value())),
        Value::Boolean(b) => Some(OptionValue::Bool(*b.value())),
        _ => None,
    }
}
```

Note the deliberate asymmetry: `number = "big"` reaches `Options::set` as
nothing at all, so it is `option_value` returning `None`. The test in Step 1
expects `number takes 0 (off), -1 (relative) or a count` for it — so route the
`None` case through `Options::set` with a value it will reject rather than
inventing a second message. Replace the `let Some(value) = … else` block with:

```rust
        let value = option_value(item);
        let result = match value {
            Some(value) => config.options.set(key, value),
            // No `OptionValue` fits, so let the option itself say what it
            // wanted. One message per option, in one place.
            None => config.options.set(key, OptionValue::Int(i64::MIN)).and_then(|()| {
                Err(format!("unknown option: {key}"))
            }),
        };
        if let Err(message) = result {
            problems.push(Diagnostic { line, message });
        }
```

- [ ] **Step 4: Declare the module**

In `src/config/mod.rs`, above the type definitions:

```rust
mod parse;

pub use parse::parse;
```

- [ ] **Step 5: Run the tests to verify they pass**

```bash
cargo test --lib config::
```

Expected: PASS — all of Task 1's, Task 2's and Task 3's tests, 14 in total.

If `a_value_of_false_is_not_yet_an_unbinding` or
`a_bad_value_reports_and_keeps_the_default` fail on the exact message, the
`OptionValue::Int(i64::MIN)` trick in Step 3 is producing the wrong branch —
`hlsearch = "yes"` must report `hlsearch takes true or false` and
`nmber = "x"` must report `unknown option: nmber`. Verify both by hand and
adjust `option_value`'s fallback so the *name* is checked before the *type*.

- [ ] **Step 6: Commit**

```bash
cargo fmt
cargo test
git add src/config/
git commit -m "config: TOML in, with a line number on everything wrong

Only malformed TOML is fatal, and even then bi starts on its defaults.
An unknown option or a value of the wrong type drops that line and says
which one it was, because a config file is edited by hand and will be
wrong sometimes — and an editor you cannot launch because of a typo in
its config is an editor you cannot use to fix the typo.

Every key belongs to a section: a bare top-level key is a diagnostic
rather than a silent acceptance, so `theme = ...` outside [ui] is caught
at the moment it is written."
```

---

### Task 4: Move options onto `Session`

**Files:**
- Modify: `src/editor.rs:477-545` (the `Session` struct), `:2169`, `:2209-2233`
- Modify: `src/tui/render.rs:221`, `:455`, `:482`, `:954-963`

**Interfaces:**
- Consumes: `Options` from Task 2
- Produces: `Session.options: Options` replacing `Session.line_numbers` and
  `Session.highlight_search`

This is a pure refactor. No behaviour changes, and no test should need a new
assertion — only a new path to the same value.

- [ ] **Step 1: Replace the two fields**

In `src/editor.rs`, delete the `highlight_search` and `line_numbers` fields
from `Session` (keeping their doc comments, which explain *why* numbering is
session-wide and why highlight is off by default) and add:

```rust
    /// Everything `:set` can change, and everything `[options]` can say.
    ///
    /// One struct rather than a field each so that a new option is one line in
    /// `Options` instead of one field here, one match arm in `set_option` and
    /// one parse rule. `:reload` also needs to replace all of them at once,
    /// which a struct does and a spray of fields does not.
    ///
    /// Session-wide by choice, where vim scopes `'number'` per window. A
    /// gutter numbered in one pane and not in its neighbour makes the same
    /// file read differently depending on where you opened it, and the setting
    /// is a reading preference rather than anything about the view. See
    /// `docs/specs/windows.md`.
    pub options: Options,
```

Add `use crate::config::Options;` to `editor.rs`'s imports.

- [ ] **Step 2: Run the build to find every reader**

```bash
cargo build 2>&1 | grep -E "^error" | head -40
```

Expected: roughly twenty errors — five readers in `render.rs`, the rest in
`editor.rs` and its tests.

- [ ] **Step 3: Update every reader**

Mechanical: `session.line_numbers` → `session.options.number`, and
`session.highlight_search` → `session.options.hlsearch`. The call sites are:

- `src/editor.rs:2169` — `ExLine::Highlight(on)`
- `src/editor.rs:2218-2222` — inside `set_option`
- `src/editor.rs:4977-4999`, `:6556-6563` — tests
- `src/tui/render.rs:221`, `:455`, `:482` — the gutter and search highlight
- `src/tui/render.rs:954-963` — tests

- [ ] **Step 4: Run the whole suite**

```bash
cargo test
```

Expected: PASS, with the same test count as before this task. A refactor that
changes a count has changed behaviour.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add src/editor.rs src/tui/render.rs
git commit -m "editor: session options are one struct

line_numbers and highlight_search were loose fields on Session, which
meant a new option touched the struct, set_option and — once a config
file exists — the parser. They move into config::Options, which is
already the one place a name becomes a field.

:reload needs to swap every option at once, and a struct is what can be
swapped."
```

---

### Task 5: `:set` goes through the options table

**Files:**
- Modify: `src/editor.rs:2209-2233` (`set_option`)

**Interfaces:**
- Consumes: `Options::set`, `Options::get`, `OptionValue`
- Produces: no new API — `set_option` shrinks to a parse and two calls

- [ ] **Step 1: Write the failing test**

In `editor.rs`'s test module, beside `set_number_takes_off_relative_and_a_count`
(around `:4975`):

```rust
#[test]
fn set_reaches_every_option_the_config_file_can() {
    let mut ed = Editor::empty();

    ex(&mut ed, "set hlsearch true");
    assert!(ed.session.options.hlsearch, "`:set` reaches an option it never used to");

    ex(&mut ed, "set hlsearch");
    assert_eq!(ed.session.status, "hlsearch=true", "and reports it back");

    ex(&mut ed, "set hlsearch false");
    assert!(!ed.session.options.hlsearch);
}

#[test]
fn set_reports_the_options_own_message_for_a_bad_value() {
    let mut ed = Editor::empty();

    ex(&mut ed, "set hlsearch maybe");
    assert_eq!(ed.session.status, "hlsearch takes true or false");

    ex(&mut ed, "set nmber 5");
    assert_eq!(ed.session.status, "unknown option: nmber");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --lib set_reaches_every_option set_reports_the_options
```

Expected: FAIL — `:set hlsearch` reports `unknown option: hlsearch`, because
`set_option`'s match still knows only `number`.

- [ ] **Step 3: Rewrite `set_option`**

Replace the body of `set_option` in `src/editor.rs` (the whole function at
`:2209`, including its doc comment about waiting for a config layer — that wait
is over):

```rust
    /// `:set <option> <value>`, or `:set <option>=<value>` — vim's spelling,
    /// which the fingers type without asking. Bare `:set <option>` reports.
    ///
    /// The names and their meanings live in [`Options`], not here, so `:set`
    /// and `config.toml` cannot disagree about what an option is or what it
    /// accepts.
    fn set_option(&mut self, arg: &str) {
        let (name, value) = match arg.split_once(['=', ' ']) {
            Some((name, value)) => (name.trim(), value.trim()),
            None => (arg.trim(), ""),
        };

        if name.is_empty() {
            self.session.status = "set what?".into();
            return;
        }

        if value.is_empty() {
            self.session.status = match self.session.options.get(name) {
                Some(OptionValue::Int(n)) => format!("{name}={n}"),
                Some(OptionValue::Bool(on)) => format!("{name}={on}"),
                None => format!("unknown option: {name}"),
            };
            return;
        }

        // The typed value `Options::set` wants. A bare word that is neither a
        // number nor a bool still goes through, so the option itself gets to
        // say what it wanted rather than this function guessing.
        let parsed = match value.parse::<i64>() {
            Ok(n) => OptionValue::Int(n),
            Err(_) => match value {
                "true" => OptionValue::Bool(true),
                "false" => OptionValue::Bool(false),
                _ => OptionValue::Int(i64::MIN),
            },
        };

        if let Err(message) = self.session.options.set(name, parsed) {
            self.session.status = message;
        }
    }
```

Add `OptionValue` to `editor.rs`'s config import:

```rust
use crate::config::{Options, OptionValue};
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test
```

Expected: PASS, including the pre-existing
`set_number_takes_off_relative_and_a_count`, which must still report
`number=1` for a bare `:set number`.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add src/editor.rs
git commit -m "editor: :set asks Options what an option is

set_option had one match arm and a comment saying a real options table
wanted the config layer. It has one now, so the arm goes: :set parses a
value and hands the name to Options::set, which is the same call the
TOML parser makes.

hlsearch becomes settable as a side effect, which is the point — the
option existed, and only :set's private list of names was keeping it
away."
```

---

### Task 6: `ConfigSource` and `Editor::load_config`

**Files:**
- Modify: `src/config/mod.rs`
- Modify: `src/editor.rs` (the `Editor` struct, `with_buffer`, a new impl block)

**Interfaces:**
- Consumes: `Config`, `parse`, `Diagnostic`
- Produces:
  - `pub trait ConfigSource { fn config(&self) -> anyhow::Result<Option<String>>; }`
  - `Editor::load_config(&mut self, source: impl ConfigSource + 'static) -> Vec<Diagnostic>`
  - `Editor::config(&self) -> &Config`

- [ ] **Step 1: Write the failing test**

In `editor.rs`'s test module:

```rust
/// A config source that serves a string, so a test needs no filesystem.
struct Text(Option<&'static str>);

impl crate::config::ConfigSource for Text {
    fn config(&self) -> anyhow::Result<Option<String>> {
        Ok(self.0.map(str::to_string))
    }
}

#[test]
fn load_config_applies_options_and_reports_problems() {
    let mut ed = Editor::empty();
    assert_eq!(ed.session.options.number, LineNumbers::Every(1), "defaults before");

    let problems = ed.load_config(Text(Some("[options]\nnumber = 5\nnmber = 9\n")));

    assert_eq!(ed.session.options.number, LineNumbers::Every(5), "the good line applied");
    assert_eq!(problems.len(), 1, "and the bad one was reported, not fatal");
    assert_eq!(problems[0].line, 3);
}

#[test]
fn no_config_file_is_not_a_problem() {
    let mut ed = Editor::empty();
    let problems = ed.load_config(Text(None));
    assert!(problems.is_empty());
    assert_eq!(ed.session.options, crate::config::Options::default());
}

#[test]
fn malformed_config_keeps_the_defaults_and_reports_once() {
    let mut ed = Editor::empty();
    let problems = ed.load_config(Text(Some("[options\nnumber = 5\n")));
    assert_eq!(problems.len(), 1);
    assert_eq!(ed.session.options.number, LineNumbers::Every(1), "unchanged");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --lib load_config no_config_file malformed_config
```

Expected: FAIL — `no method named load_config`.

- [ ] **Step 3: Add the trait**

In `src/config/mod.rs`:

```rust
/// Where a frontend gets config text from.
///
/// A trait rather than a path, because "where does a config file live on this
/// platform, or inside this embedding host" is the one part of config that
/// genuinely varies per frontend — and the library must not learn what a
/// filesystem is to serve it. An embedder can read from a database or a
/// bundled resource.
///
/// `Ok(None)` means there is no config, which is normal and not a problem.
/// `Err` means there was one and it could not be read, which is.
pub trait ConfigSource {
    fn config(&self) -> anyhow::Result<Option<String>>;
}
```

- [ ] **Step 4: Give `Editor` the config and the source**

In `src/editor.rs`, add two fields to the `Editor` struct:

```rust
    config: Config,
    /// Kept so `:reload` can ask again. `None` until a frontend supplies one —
    /// an embedder that wants no config never calls `load_config`, and
    /// `:reload` then has nothing to re-read and says so.
    config_source: Option<Box<dyn ConfigSource>>,
```

In `with_buffer`, initialise them:

```rust
            config: Config::default(),
            config_source: None,
```

Import them: `use crate::config::{Config, ConfigSource, Options, OptionValue};`

- [ ] **Step 5: Implement `load_config`**

Add to the `impl Editor` block, near `open` and `empty`:

```rust
    /// Applies a config source, and remembers it for `:reload`.
    ///
    /// Called after construction rather than passed to [`Editor::open`] so
    /// that the three dozen existing call sites — nearly all tests — stay as
    /// they are, and so an embedder that wants no config simply never calls
    /// it.
    ///
    /// The returned diagnostics are the frontend's to show. Startup and
    /// `:reload` run this same path, which is the only way the two stay in
    /// agreement.
    pub fn load_config(&mut self, source: impl ConfigSource + 'static) -> Vec<Diagnostic> {
        let problems = self.read_config(&source);
        self.config_source = Some(Box::new(source));
        problems
    }

    /// The config bi is running on.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Reads and applies, without touching the stored source. Shared by
    /// [`Editor::load_config`] and `:reload`.
    fn read_config(&mut self, source: &dyn ConfigSource) -> Vec<Diagnostic> {
        let text = match source.config() {
            Ok(Some(text)) => text,
            Ok(None) => return Vec::new(),
            Err(e) => return vec![Diagnostic { line: 1, message: e.to_string() }],
        };

        match crate::config::parse(&text, Config::default()) {
            Ok((config, problems)) => {
                self.apply_config(config);
                problems
            }
            // Unsalvageable: the running config stays exactly as it was.
            Err(problem) => vec![problem],
        }
    }

    fn apply_config(&mut self, config: Config) {
        self.session.options = config.options;
        self.config = config;
    }
```

Import `Diagnostic` alongside the rest.

- [ ] **Step 6: Run the tests to verify they pass**

```bash
cargo test
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
cargo fmt
git add src/config/mod.rs src/editor.rs
git commit -m "editor: a config source, and the one path that reads it

The library must not learn what a filesystem is, and the frontend must
not learn what a keymap is, so config arrives through a trait: the
frontend answers \"here is the text\", and everything downstream of that
is library.

read_config is deliberately shared with :reload rather than duplicated.
Startup and reload disagreeing about what a config file means is the
failure this shape exists to prevent."
```

---

### Task 7: Rename `ExLine::Reload` to `ExLine::Revert`

**Files:**
- Modify: `src/editor.rs:574`, `:611`, `:684`, `:2189`

**Interfaces:**
- Consumes: nothing
- Produces: `ExLine::Revert` — frees the name `Reload` for Task 8

Pure rename. `ExLine::Reload` is bare `:e`: re-read the *buffer* from disk. Two
things called reload meaning two different jobs is a trap for whoever reads
`editor.rs` next.

- [ ] **Step 1: Rename**

```bash
grep -rn "ExLine::Reload" src/
```

Rename every occurrence to `ExLine::Revert`, including the variant definition at
`:611` and the doc comment at `:574` that says "Bare `:e` is [`ExLine::Reload`]".
Update that comment to name `Revert` and to say why:

```rust
    /// `:e <path>`. Bare `:e` is [`ExLine::Revert`], which is a different job
    /// — and is not `:reload`, which is the config.
```

- [ ] **Step 2: Run the tests**

```bash
cargo test
```

Expected: PASS, unchanged count. `:e` and `:e!` behave exactly as before.

- [ ] **Step 3: Commit**

```bash
cargo fmt
git add src/editor.rs
git commit -m "editor: :e's reload is a revert

Bare :e re-reads the buffer from disk, which is vim's :e! semantics and
is not what :reload is about to mean. Renamed before the collision
exists rather than after, so nobody has to hold two reloads in their
head at once."
```

---

### Task 8: `:reload`

**Files:**
- Modify: `src/editor.rs` — `ExLine` enum, `parse_ex` (`:656`), the `run_ex`
  match, and a new `Editor::reload_config`

**Interfaces:**
- Consumes: `Editor::read_config`, `Editor.config_source`
- Produces: `ExLine::ReloadConfig`; `Editor::reload_config(&mut self)`

- [ ] **Step 1: Write the failing test**

In `editor.rs`'s test module:

```rust
/// A source whose text can change between reads, which is what `:reload` is
/// for. `Cell` rather than a field because `ConfigSource::config` takes `&self`
/// — the source is read-only to the editor, and mutable only to its owner.
struct Mutable(std::cell::RefCell<String>);

impl crate::config::ConfigSource for Mutable {
    fn config(&self) -> anyhow::Result<Option<String>> {
        Ok(Some(self.0.borrow().clone()))
    }
}

#[test]
fn reload_picks_up_a_changed_file() {
    let mut ed = Editor::empty();
    let source = std::rc::Rc::new(Mutable("[options]\nnumber = 5\n".to_string().into()));
    ed.load_config(std::rc::Rc::clone(&source));
    assert_eq!(ed.session.options.number, LineNumbers::Every(5));

    *source.0.borrow_mut() = "[options]\nnumber = -1\n".to_string();
    ex(&mut ed, "reload");

    assert_eq!(ed.session.options.number, LineNumbers::Relative);
    assert_eq!(ed.session.status, "config reloaded");
}

#[test]
fn a_failed_reload_changes_nothing() {
    let mut ed = Editor::empty();
    let source = std::rc::Rc::new(Mutable("[options]\nnumber = 5\n".to_string().into()));
    ed.load_config(std::rc::Rc::clone(&source));

    *source.0.borrow_mut() = "[options\nnumber = -1\n".to_string();
    ex(&mut ed, "reload");

    assert_eq!(ed.session.options.number, LineNumbers::Every(5), "the running config survives");
    assert!(ed.session.status.contains("config not reloaded"), "{}", ed.session.status);
}

#[test]
fn reload_counts_the_problems_it_kept_going_past() {
    let mut ed = Editor::empty();
    let source = std::rc::Rc::new(Mutable("[options]\nnmber = 9\nzz = 1\n".to_string().into()));
    ed.load_config(std::rc::Rc::clone(&source));

    ex(&mut ed, "reload");
    assert_eq!(ed.session.status, "config reloaded — 2 problems");
}

#[test]
fn reload_without_a_source_says_so() {
    let mut ed = Editor::empty();
    ex(&mut ed, "reload");
    assert_eq!(ed.session.status, "no config to reload");
}
```

`Rc<Mutable>` needs a blanket impl so a shared handle is still a source. Add it
in `src/config/mod.rs`:

```rust
impl<T: ConfigSource> ConfigSource for std::rc::Rc<T> {
    fn config(&self) -> anyhow::Result<Option<String>> {
        (**self).config()
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --lib reload
```

Expected: FAIL — `:reload` reports `not a command: reload`.

- [ ] **Step 3: Add the ex command**

In the `ExLine` enum:

```rust
    /// `:reload` — the config, not the buffer. See [`ExLine::Revert`].
    ReloadConfig,
```

In `parse_ex`, beside the other name arms:

```rust
        "reload" => ExLine::ReloadConfig,
```

In `run_ex`'s match, with the arms that need no view:

```rust
            ExLine::ReloadConfig => self.reload_config(),
```

- [ ] **Step 4: Implement `reload_config`**

Add to `impl Editor`, beside `load_config`:

```rust
    /// Re-reads the config through the source a frontend supplied, and swaps
    /// every option at once.
    ///
    /// A failed reload changes nothing. Reloading yourself into an unusable
    /// config, with no way to type `:reload` again, is the one outcome worth
    /// engineering against — so a document that does not parse is reported and
    /// discarded, and the running config stays.
    fn reload_config(&mut self) {
        // Taken and put back: `read_config` needs `&mut self`, and the source
        // lives on `self`.
        let Some(source) = self.config_source.take() else {
            self.session.status = "no config to reload".into();
            return;
        };

        let problems = self.read_config(source.as_ref());
        self.config_source = Some(source);

        self.session.status = match problems.len() {
            0 => "config reloaded".into(),
            n => format!("config reloaded — {n} problem{}", if n == 1 { "" } else { "s" }),
        };
    }
```

`read_config` must distinguish "the document did not parse" from "some lines
were bad", because the status differs. Change its `Err` arm to report against
the running config:

```rust
            Err(problem) => {
                self.session.status =
                    format!("config not reloaded — line {}: {}", problem.line, problem.message);
                vec![problem]
            }
```

and have `reload_config` leave that status alone when it happened:

```rust
        let fatal = problems.len() == 1 && self.session.status.starts_with("config not reloaded");
        self.config_source = Some(source);
        if fatal {
            return;
        }
```

- [ ] **Step 5: Run the tests to verify they pass**

```bash
cargo test
```

Expected: PASS.

If `a_failed_reload_changes_nothing` passes but the status is wrong, prefer
making `read_config` return a `Result<Vec<Diagnostic>, Diagnostic>` over the
`starts_with` sniff above — it is the honest shape, and the sniff is only
cheaper. Either is acceptable; a string comparison deciding control flow is
worth replacing if it costs more than five minutes.

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add src/config/mod.rs src/editor.rs
git commit -m "editor: :reload re-reads the config

Through the same source and the same path as startup, because startup
and reload disagreeing about what a config file means is exactly the bug
this avoids.

A reload that fails changes nothing. Landing in an unusable config with
no way to type :reload again is the one outcome worth engineering
against, so a document that will not parse is reported and dropped."
```

---

### Task 9: XDG discovery in the frontend

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `ConfigSource`, `Editor::load_config`
- Produces: `config_dir() -> Option<PathBuf>`; `struct XdgConfig`

- [ ] **Step 1: Write the failing test**

Add a test module at the bottom of `src/main.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_dir_prefers_bi_config_then_xdg_then_home() {
        let bi = dir_from(Some("/explicit"), Some("/xdg"), Some("/home"));
        assert_eq!(bi, Some(PathBuf::from("/explicit")));

        let xdg = dir_from(None, Some("/xdg"), Some("/home"));
        assert_eq!(xdg, Some(PathBuf::from("/xdg/bi")));

        let home = dir_from(None, None, Some("/home"));
        assert_eq!(home, Some(PathBuf::from("/home/.config/bi")));

        assert_eq!(dir_from(None, None, None), None, "nowhere to look is not a crash");
    }

    #[test]
    fn an_empty_env_var_is_the_same_as_an_unset_one() {
        assert_eq!(dir_from(Some(""), None, Some("/home")), Some(PathBuf::from("/home/.config/bi")));
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test --bin bi
```

Expected: FAIL — `cannot find function dir_from`.

- [ ] **Step 3: Implement discovery**

Add to `src/main.rs`:

```rust
use std::path::PathBuf;

use bi::config::ConfigSource;

/// bi's config directory: `$BI_CONFIG`, else `$XDG_CONFIG_HOME/bi`, else
/// `~/.config/bi`.
///
/// A directory rather than a file, because `themes/` is its sibling and
/// `bi config edit` opens the lot. This is the whole of what the frontend
/// knows that the library does not.
fn config_dir() -> Option<PathBuf> {
    dir_from(
        std::env::var("BI_CONFIG").ok().as_deref(),
        std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

/// The rule, with the environment passed in so it can be tested without
/// setting process-wide variables — which two tests running at once would
/// fight over.
fn dir_from(bi: Option<&str>, xdg: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    let some = |s: Option<&str>| s.filter(|s| !s.is_empty());

    if let Some(explicit) = some(bi) {
        return Some(PathBuf::from(explicit));
    }
    if let Some(xdg) = some(xdg) {
        return Some(PathBuf::from(xdg).join("bi"));
    }
    some(home).map(|home| PathBuf::from(home).join(".config").join("bi"))
}

/// Reads bi's config off the filesystem.
struct XdgConfig {
    dir: Option<PathBuf>,
}

impl ConfigSource for XdgConfig {
    fn config(&self) -> Result<Option<String>> {
        let Some(dir) = &self.dir else { return Ok(None) };
        let path = dir.join("config.toml");
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(Some(text)),
            // No config file is the normal case, not a problem.
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).context(format!("reading {}", path.display())),
        }
    }
}
```

- [ ] **Step 4: Load it at startup**

In `main`, after the editor is built and before `setup()`:

```rust
    let problems = editor.load_config(XdgConfig { dir: config_dir() });
```

And after `setup()` succeeds, put the count where the user will see it —
diagnostics go to the status line, not stderr, because the alternate screen
swallows stderr:

```rust
    if !problems.is_empty() {
        let n = problems.len();
        editor.session.status =
            format!("{n} config problem{}: {}", if n == 1 { "" } else { "s" }, problems[0].message);
    }
```

- [ ] **Step 5: Run the tests and try it by hand**

```bash
cargo test
mkdir -p /tmp/bi-config-check
printf '[options]\nnumber = -1\n' > /tmp/bi-config-check/config.toml
BI_CONFIG=/tmp/bi-config-check cargo run -- src/main.rs
```

Expected: the gutter shows relative numbers. `:set number` reports `number=-1`.
Quit with `:q`.

Then check the failure path:

```bash
printf '[options]\nnmber = 1\n' > /tmp/bi-config-check/config.toml
BI_CONFIG=/tmp/bi-config-check cargo run -- src/main.rs
```

Expected: bi starts, gutter is normal, status line reads
`1 config problem: unknown option: nmber`.

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add src/main.rs
git commit -m "bi: find the config, and say when it is wrong

\$BI_CONFIG, else \$XDG_CONFIG_HOME/bi, else ~/.config/bi — a
directory, because themes/ is its sibling and \`bi config edit\` opens
the lot.

Discovery is the frontend's whole part in config: a GUI or an embedding
host would answer this question differently and nothing else. The rule
takes its environment as arguments so it can be tested without two
tests fighting over the same process-wide variables.

Problems go to the status line. The alternate screen swallows stderr,
which is where a config warning would otherwise die unread."
```

---

### Task 10: `bi config init`

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `bi::config::DEFAULT_TOML`, `config_dir()`
- Produces: `enum Invocation`; `parse_args(&[String]) -> Result<Invocation>`;
  `commented(defaults: &str) -> String`; `config_init(dir) -> Result<()>`

- [ ] **Step 1: Write the failing tests**

In `main.rs`'s test module:

```rust
#[test]
fn args_route_the_two_subcommands_and_nothing_else() {
    let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();

    assert!(matches!(parse_args(&args(&[])).unwrap(), Invocation::Open(None)));
    assert!(matches!(parse_args(&args(&["a.rs"])).unwrap(), Invocation::Open(Some(_))));
    assert!(matches!(parse_args(&args(&["config"])).unwrap(), Invocation::Open(Some(_))),
        "a file named `config` still opens; the subcommand form takes two words");
    assert!(matches!(parse_args(&args(&["config", "init"])).unwrap(), Invocation::ConfigInit));
    assert!(matches!(parse_args(&args(&["config", "edit"])).unwrap(), Invocation::ConfigEdit));
    assert!(parse_args(&args(&["config", "nope"])).is_err());
    assert!(parse_args(&args(&["a.rs", "b.rs"])).is_err());
}

#[test]
fn the_written_config_is_the_defaults_commented_out() {
    let out = commented("[options]\nnumber = 1\n\n# already a comment\n");

    assert!(out.contains("# [options]"), "settings are commented: {out}");
    assert!(out.contains("# number = 1"));
    assert!(out.contains("# already a comment"), "and not double-commented");
    assert!(!out.contains("## already"), "{out}");

    // The whole file must be inert, or a user's config silently becomes a
    // full replacement and they stop receiving later defaults.
    let (config, problems) = bi::config::parse(&out, bi::config::Config::default()).unwrap();
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(config, bi::config::Config::default(), "semantically empty");
}

#[test]
fn init_writes_once_and_never_overwrites() {
    let dir = std::env::temp_dir().join(format!("bi-init-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    config_init(&dir).unwrap();
    let first = std::fs::read_to_string(dir.join("config.toml")).unwrap();
    assert!(first.contains("PATCH over bi's defaults"));

    std::fs::write(dir.join("config.toml"), "mine\n").unwrap();
    config_init(&dir).unwrap();
    assert_eq!(std::fs::read_to_string(dir.join("config.toml")).unwrap(), "mine\n",
        "a second init leaves the user's file alone");

    std::fs::remove_dir_all(&dir).unwrap();
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --bin bi
```

Expected: FAIL — `cannot find function parse_args`.

- [ ] **Step 3: Implement argument routing**

In `src/main.rs`:

```rust
/// What the command line asked for.
enum Invocation {
    Open(Option<String>),
    ConfigInit,
    ConfigEdit,
}

/// `config` is a subcommand only in the two-word form, so a file actually
/// named `config` still opens.
fn parse_args(args: &[String]) -> Result<Invocation> {
    match args {
        [] => Ok(Invocation::Open(None)),
        [one] => Ok(Invocation::Open(Some(one.clone()))),
        [first, sub] if first == "config" => match sub.as_str() {
            "init" => Ok(Invocation::ConfigInit),
            "edit" => Ok(Invocation::ConfigEdit),
            other => bail!("no such command: bi config {other} — try `init` or `edit`"),
        },
        _ => bail!("usage: bi [path] | bi config init | bi config edit"),
    }
}
```

Add `use anyhow::bail;` to the existing `anyhow` import.

- [ ] **Step 4: Implement the writer**

```rust
/// The header on a freshly written config, explaining the one thing a user
/// has to know about the file.
const INIT_HEADER: &str = "\
# bi config
#
# This file is a PATCH over bi's defaults, not a replacement. Anything left
# commented out keeps doing what bi does by default, including settings added
# in later versions. Uncomment a line only to change it.
#
# `:reload` re-reads this file without restarting.

";

/// bi's defaults, commented out.
///
/// Written live they would silently turn every user's file into a full
/// replacement, and that user would stop receiving defaults bi adds later —
/// invisibly and permanently. Commented out it is a self-documenting menu that
/// is semantically empty.
fn commented(defaults: &str) -> String {
    let mut out = String::from(INIT_HEADER);
    for line in defaults.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            out.push('\n');
        } else if trimmed.starts_with('#') {
            out.push_str(line);
            out.push('\n');
        } else {
            out.push_str("# ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Creates the config directory and writes `config.toml` if it is absent.
///
/// Never automatic: a config file appears because you asked for one.
fn config_init(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;

    let path = dir.join("config.toml");
    if path.exists() {
        println!("{} already exists — leaving it alone", path.display());
        return Ok(());
    }

    std::fs::write(&path, commented(bi::config::DEFAULT_TOML))
        .with_context(|| format!("writing {}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(())
}
```

Add `use std::path::Path;`.

- [ ] **Step 5: Route it in `main`**

Restructure `main` so the terminal is only entered for the editing paths:

```rust
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let path = match parse_args(&args)? {
        Invocation::ConfigInit => {
            let dir = config_dir().context("no HOME and no XDG_CONFIG_HOME — nowhere to write")?;
            return config_init(&dir);
        }
        Invocation::ConfigEdit => unimplemented!("Task 11"),
        Invocation::Open(path) => path,
    };

    let mut editor = match path {
        Some(path) => Editor::open(path)?,
        None => Editor::empty(),
    };

    let problems = editor.load_config(XdgConfig { dir: config_dir() });

    let mut term = setup().context("entering raw mode")?;
    if !problems.is_empty() {
        let n = problems.len();
        editor.session.status =
            format!("{n} config problem{}: {}", if n == 1 { "" } else { "s" }, problems[0].message);
    }
    let result = run(&mut term, &mut editor);
    restore()?;
    result
}
```

- [ ] **Step 6: Run the tests and try it by hand**

```bash
cargo test
BI_CONFIG=/tmp/bi-init-demo cargo run -- config init
cat /tmp/bi-init-demo/config.toml
BI_CONFIG=/tmp/bi-init-demo cargo run -- config init
```

Expected: the first writes and prints `wrote …`; the second prints
`already exists — leaving it alone`. The file's settings are all commented.

- [ ] **Step 7: Commit**

```bash
cargo fmt
rm -rf /tmp/bi-init-demo /tmp/bi-config-check
git add src/main.rs
git commit -m "bi: config init writes the defaults, commented out

Manual, never automatic — a config file appears because you asked for
one.

It writes every default commented out rather than live. Written live,
the file would silently become a full replacement of the defaults, and
that user would stop receiving anything bi adds later — invisibly, and
permanently. Commented out it is a documented menu that is semantically
empty, which a test asserts by parsing it and comparing to the defaults."
```

---

### Task 11: `bi config edit`

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `Invocation::ConfigEdit`, `config_dir()`
- Produces: nothing new — `Editor::open` already opens a directory as a tree

- [ ] **Step 1: Write the failing test**

In `main.rs`'s test module:

```rust
#[test]
fn edit_refuses_a_directory_that_does_not_exist() {
    let missing = std::env::temp_dir().join(format!("bi-absent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&missing);

    let err = config_edit_path(&missing).expect_err("nothing to edit yet");
    assert!(err.to_string().contains("bi config init"), "{err}");
}

#[test]
fn edit_opens_the_directory_it_finds() {
    let dir = std::env::temp_dir().join(format!("bi-edit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    config_init(&dir).unwrap();

    assert_eq!(config_edit_path(&dir).unwrap(), dir, "the directory, so themes/ is in the tree");

    std::fs::remove_dir_all(&dir).unwrap();
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --bin bi
```

Expected: FAIL — `cannot find function config_edit_path`.

- [ ] **Step 3: Implement it**

```rust
/// What `bi config edit` opens: the config *directory*, so `themes/` is in
/// the tree beside `config.toml`.
///
/// It does not create anything. `bi config init` is the manual step, and
/// `edit` surprising you with a new file would undo that.
fn config_edit_path(dir: &Path) -> Result<PathBuf> {
    if !dir.exists() {
        bail!("no config yet — run `bi config init`");
    }
    Ok(dir.to_path_buf())
}
```

- [ ] **Step 4: Route it in `main`**

Replace the `unimplemented!` from Task 10:

```rust
        Invocation::ConfigEdit => {
            let dir = config_dir().context("no HOME and no XDG_CONFIG_HOME — nowhere to look")?;
            Some(config_edit_path(&dir)?.to_string_lossy().into_owned())
        }
```

`Editor::open` opens a directory as a tree (`src/editor.rs:947`), so nothing
else is needed.

- [ ] **Step 5: Run the tests and try it by hand**

```bash
cargo test
BI_CONFIG=/tmp/bi-edit-demo cargo run -- config edit
```

Expected: `Error: no config yet — run \`bi config init\``, exit non-zero.

```bash
BI_CONFIG=/tmp/bi-edit-demo cargo run -- config init
BI_CONFIG=/tmp/bi-edit-demo cargo run -- config edit
```

Expected: bi opens with a file tree showing `config.toml`. Press `<CR>` on it,
edit `number = 1` to `number = -1` uncommented, `:w`, then `:reload` — the
gutter switches to relative numbers without restarting. That is the whole
feature working end to end.

- [ ] **Step 6: Commit**

```bash
cargo fmt
rm -rf /tmp/bi-edit-demo
git add src/main.rs
git commit -m "bi: config edit opens the config directory

Editor::open already opens a directory as a tree, so this is argument
routing and no new editor code — and themes/ comes along in the same
tree, which is the point of pointing it at the directory rather than the
file.

It does not create anything. init is the manual step, and an edit that
surprised you with a new file would undo that."
```

---

### Task 12: Update the docs

**Files:**
- Modify: `README.md` — "Ex commands" (`:294`), "Known gaps" (`:524`),
  "Architectural, and cheaper to fix now than later" (`:549`), "Next" (`:576`)
- Modify: `docs/specs/config.md` — the Status section

**Interfaces:** none

- [ ] **Step 1: Record the ex commands**

In README's "Ex commands" table, add `:reload` beside `:e`, saying which is
which: `:e` re-reads the buffer, `:reload` re-reads the config.

- [ ] **Step 2: Retire the resolved gap**

In "Architectural, and cheaper to fix now than later", the bullet beginning
**The config language is undecided** is now half wrong. Strike it through the
way the cursor-on-`Buffer` bullet was struck through when `Selections` landed:

```markdown
- ~~**The config language is undecided**~~ Decided and half built: TOML, in
  `~/.config/bi/config.toml`, parsed by `bi::config`. `[options]` is live.
  The keymap in `input.rs` and the highlight table in `tui/render.rs` are still
  hardcoded and are steps 3 and 2 of [docs/specs/config.md](docs/specs/config.md).
```

- [ ] **Step 3: Add a config section to the README**

After "Line numbers" (`:420`), a short section: where the file lives, that it
is a patch over the defaults, `bi config init` / `edit`, `:reload`, and the
`[options]` table with its two entries. Match the surrounding voice — the
README explains *why* a thing is shaped the way it is, not only what it does.

- [ ] **Step 4: Rewrite "Next"**

The current text says the config decision is unmade. Replace it with: the theme
(step 2) and the keymap (step 3), then LSP.

- [ ] **Step 5: Update the spec's Status**

In `docs/specs/config.md`, change **Specified, not built.** to:

```markdown
**Step 1 built.**

The layer, `[options]`, `:reload` and both CLI subcommands ship. The theme
(step 2) and the keymap (step 3) are specified below and not yet built.
```

- [ ] **Step 6: Verify and commit**

```bash
cargo test
cargo fmt --check
git add README.md docs/specs/config.md
git commit -m "docs: the config layer, and one fewer architectural gap

RECOMMENDATION.md has called the config language the painful retrofit
since the first commit, and README has carried it as the next step for
three. It is decided now, and a third of it is built."
```

---

## Self-Review

**Spec coverage.** Every requirement of the spec's step 1 has a task: XDG
discovery (9), the section rule and `[options]` (3), patch-over-defaults (3),
`ConfigSource` and the boundary (6), non-fatal diagnostics (3, 6, 9), `:reload`
with the `Revert` rename (7, 8), `bi config init` (10) and `edit` (11), and
`[options]` unified with `:set` (4, 5). Steps 2 (theme) and 3 (keymap) are out
of scope by the Global Constraints and get their own plans.

**Known rough edges, called out where they occur:**

- Task 1 Step 6 and Task 3 Step 5 both name a specific failure and what to do
  about it. `toml_edit`'s span API is the one dependency detail this plan
  asserts without having run it.
- Task 2's commit does not build on its own. The step says so and offers the
  alternative.
- Task 8 Step 4 uses a `starts_with` check on a status string to detect a fatal
  reload, and Step 5 says to replace it with a `Result` if it is not free. It is
  the one place in this plan where the shape is knowingly not the best one.

**Type consistency.** `Options::set` / `Options::get` / `OptionValue` are
defined in Task 2 and used unchanged in Tasks 3 and 5. `parse` is defined in
Task 3 with the `Result<(Config, Vec<Diagnostic>), Diagnostic>` signature and
used with that signature in Tasks 6 and 10. `ConfigSource::config` returns
`anyhow::Result<Option<String>>` in Tasks 6, 8, 9. `Session.options` replaces
the two loose fields in Task 4 and is read in Tasks 5, 6, 8.
