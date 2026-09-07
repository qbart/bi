# Transient Buffers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `:!` (and `:w !`) output lands in a transient buffer `[!cmd]` — a real, unsaveable buffer — instead of the debugger's Console pane; `:bd` on it stops a running job.

**Architecture:** `Buffer` gains `kind: Kind { File, Transient { name } }` beside `path`; a transient buffer is an ordinary buffer with save refused, nags skipped, and a line cap. The editor gets a find-or-create `transient_buffer(name)`, a `show_transient` (reuse a window or split below, focus kept), and `append_to_transient` with the cursor-follows-tail rule. The shell's three sinks switch from the Console to those. The debugger's `DapConsole` pane is untouched.

**Spec:** `docs/specs/transient.md` (binding; also amends `docs/specs/shell.md` §Jobs)

## Global Constraints

- Transient buffer: no path, never adopts one. `:w` → status `transient — :w <path> saves a copy`; `:w <path>` writes the text, buffer stays transient; `:wa` skips it. `:bd`/`:q`/`:qa` never nag about it. No LSP (path None already), no git baseline, no trim-on-write, no editorconfig. Plain-text syntax.
- `TRANSIENT_MAX_LINES = 10_000`; appending past it drops lines from the top.
- `:!`/`:!!`/`:w !` append to ONE transient buffer named `[!<latest cmd>]` (renamed per run), under a fresh `$ <cmd>` header; stderr `! `-prefixed; trailer `exited n`/`killed`/`signal`; status line unchanged (`! cmd` / `! exited n`). No pane title.
- Show: reuse any window showing the buffer, else split BELOW the current window; FOCUS STAYS in the user's buffer.
- Cursor follows the tail only if it was ON the last line before the append (per window).
- `:bd` on the job's buffer while running: stop the job (kill), then close. `:stop` unchanged. Quit kills.
- Remove the Console's `Ctrl-C`/`x` stop keys and the shell pane title; the debugger's Console pane, `:debug console`, output events and `:eval` are UNTOUCHED.
- House style; TDD; `cargo test` + `cargo clippy --all-targets` clean per commit; short commits, no AI trailers; master.

---

### Task 1: `Buffer::kind`, `Buffer::transient`, `append_lines` with cap, save rules (`src/buffer.rs`)
**Produces:** `pub enum Kind { File, Transient { name: String } }` (derive Debug/Clone/PartialEq/Eq); `pub kind: Kind` on `Buffer` (File for `empty()`/`open()`); `pub fn transient(name: &str) -> Buffer`; `pub fn is_transient(&self) -> bool`; `pub fn transient_name(&self) -> Option<&str>`; `pub fn set_transient_name(&mut self, name)`; `pub const TRANSIENT_MAX_LINES: usize = 10_000`; `pub fn append_lines(&mut self, lines: &[String]) -> Option<usize>` (appends each line + `\n` at the end as ONE edit through the normal edit path so windows/undo/LSP-drain see it; then trims from the top past the cap; returns the row of the first appended line, `None` if empty); `save()` on a transient → `Err("transient — :w <path> saves a copy")`; `save_as(path)` on a transient writes the file but does NOT set `path`/clear modified (say why in the doc). Read `save`/`save_as`/`edit_raw`/how `Editor::empty` builds a buffer first.
- [ ] Tests (buffer.rs): `a_transient_buffer_refuses_a_bare_save`, `save_as_writes_a_copy_and_stays_transient` (tempdir), `append_lines_adds_at_the_end_as_one_undo` (append 3 lines → one `undo` restores), `appending_past_the_cap_drops_from_the_top` (cap+5 → exactly cap lines, the oldest gone; use a small test-only cap via a `#[cfg(test)]` helper or parameterise `append_lines_capped(lines, cap)` with `append_lines` calling it — say which), `transient_name_renames`.
- [ ] RED → GREEN → full suite + clippy → commit `buffer: transient kind, append with a cap, save refused`.

### Task 2: Editor surface — find/create, show, append-with-follow, nags, `:w`, `name_of` (`src/editor.rs`)
**Produces:** `pub fn transient_buffer(&mut self, name: &str) -> BufferId` (the single existing transient buffer renamed, else a new `BufferEntry` from `Buffer::transient(name)` with `syntax: None`, `filetype: None`, no git, no lsp); `fn show_transient(&mut self, id)` (a window already showing `id` → nothing; else split BELOW the focused window showing `id` — mirror `WindowCmd::New { dir: Horizontal }`'s mechanics (~5786) but with `id` instead of a fresh empty buffer — and restore focus); `fn append_to_transient(&mut self, id, lines: &[String])` (records, per window on `id`, whether the cursor row == last row; appends via `Buffer::append_lines`; for windows that were at the tail, moves the cursor to the new last row and scrolls; `settle()` is what drains the edit to other windows — call the same remap the drain does, or rely on `settle`; say which). Nags: `delete_buffer` (~4528) and `quit_all` (~6314) skip transient buffers (`is_modified && !is_transient`); `:wa` skips them; `:w` with no arg on a transient → the copy hint (find the `no file name` site ~4593); `:w <path>` → `save_as` copy; `name_of` (~4199) → `[name]` for transient. `attach_lsp`/git baseline/trim: verify they already skip a pathless buffer; add an explicit `is_transient` skip where a path-less buffer would otherwise be touched (trim-on-write runs in `write` — a transient never reaches `save`, but `save_as` does: don't trim a copy).
- [ ] Tests (`mod transient` in editor.rs): `a_transient_buffer_shows_in_a_split_below_and_keeps_focus`, `append_follows_a_cursor_on_the_last_line_and_leaves_one_that_moved_up` (two windows on it), `bare_w_is_refused_with_the_copy_hint`, `w_path_writes_a_copy_and_the_buffer_stays_transient` (ScratchDir), `bd_and_q_do_not_nag_about_a_transient_buffer`, `wa_skips_it`, `ls_shows_the_bracketed_name`, `no_lsp_git_or_trim_for_a_transient` (set fakes; assert no spawn/baseline; `save_as` doesn't trim trailing whitespace).
- [ ] RED → GREEN → full + clippy → commit `editor: transient buffers — show below, append with follow, no nags`.

### Task 3: `:!` and `:w !` into the transient buffer; `:bd` stops the job; remove the Console keys/title
**Files:** `src/editor.rs` (`run_bang`, `run_write`, `pump_shell`, `stop_shell`, `delete_buffer`, `shell_title`), `src/input.rs` (the Console `Ctrl-C`/`x` arm + its 3 tests), `src/tui/render.rs` (the `shell_title` use in the Console title arm — restore the pre-shell title comment/behaviour for the debugger pane).
- `run_bang`: after expand/refuse, `let id = self.transient_buffer(&format!("!{cmd}"))`; `append_to_transient(id, ["$ cmd"])`; `show_transient(id)`; spawn; store `job_buffer: Some(id)` on the job (or beside `shell`). `pump_shell`: append Out/`! `Err lines and the trailer to `job_buffer` (if the buffer was `:bd`-closed mid-run, drop lines and just set the status). `run_write`: same buffer. `delete_buffer(id)`: if `id` is the running job's buffer → `stop_shell()` first (kill), then proceed. `shell_title()` removed; the Console title arm in render.rs goes back to plain Console (the debugger's). input.rs: remove the Console `Ctrl-C`/`x` → `Stop` arm and its tests (`x` in a Console pane is the tree grammar again as before the shell feature; `Ctrl-C` in text unchanged); keep `ShellCmd::Stop` reachable via `:stop` only.
- [ ] Tests: rewrite `shell_integration`'s console assertions to the transient buffer (`$ echo hi`/`hi`/`exited 0` in `[!echo hi]`; second `:!` appends + renames; stderr `! `; focus unchanged; `:bd` on the running job's buffer → `fake.killed == 1` and the buffer is gone; `:stop` unchanged; `:w !cat` output in the same buffer; the pane-title test (E1) DELETED — assert the status line instead). Regression: the debugger's `output_events_append_to_the_console` / `eval_appends_the_answer_to_the_console` still pass untouched.
- [ ] RED → GREEN → full + clippy → commit `:! writes to a transient buffer; :bd stops the job`.

### Task 4: Docs
- `docs/specs/transient.md` → Built; `docs/specs/shell.md`: §Jobs "Output goes to the debugger's Console pane" → the transient buffer (link), §Stopping drop the pane keys, Deviations updated (Console reuse reversed), Tests bullets; `docs/specs/debug.md` §Console: remove "`:!` shares this pane"; `docs/GENERAL.md`: `:!` rows/Shell para + a Transient buffers sentence under buffers. Commit `transient: docs, built`.

## Self-review
Spec coverage: kind/save/nag/cap (T1), show-below/focus/follow/name/`:w` copy/no-attach (T2), `:!` sinks/`:bd` stops/keys+title removed/debugger untouched (T3), docs (T4). Every Tests bullet maps to a test. Types: `Kind`, `Buffer::transient`, `append_lines`, `TRANSIENT_MAX_LINES`, `transient_buffer`, `show_transient`, `append_to_transient` used identically.
