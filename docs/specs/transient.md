# Transient buffers

`:!make` puts its output in a buffer, not a pane. A buffer you can read, yank
from, search, edit, split, switch to with `Ctrl-^`, and close with `:bd` —
everything a buffer does — except save. That is the whole idea, and it is
vim's `buftype=nofile`.

## Status

**Approved for build.** Decisions resolved with the user on 2026-09-07; see
the end.

## Why

`docs/specs/shell.md` sent `:!` output to the debugger's Console pane. That
pane is a window *content kind*, like the file tree: you can focus it, but it
is not a buffer. It cannot be `:bd`-closed, edited, or yanked from; its keys
are its own; and when the process behind it dies out from under it there is
nothing left to close but the window. A build log is text. Text lives in a
buffer.

The debugger's own Console stays exactly as it is — this spec is about `:!`,
and only `:!`.

## What a transient buffer is

A [`Buffer`] whose `kind` is `Transient { name }`, shown as `[!make]` in
`:ls`, the status line and the switcher. It has no path and never gets one.
Everything a buffer already does, it does: Normal, Insert and Visual modes,
motions, operators, registers, search, undo, splits, `gb`, `Ctrl-^`.

What it does not do:

- **Save.** `:w` says `transient — :w <path> saves a copy`. `:w <path>` writes
  the text to that path and the buffer stays transient — the log was copied,
  not adopted. `:wa` skips it.
- **Nag.** `:bd` and `:q` never say "unsaved changes" about it: there is no
  file to be behind, so nothing is lost. `:bd!` is not needed.
- **Attach.** No LSP server (no path, no URI — already the rule), no git
  signs, no trim-on-write, no `.editorconfig` lookup (no path to look up
  from). Syntax is plain text.

**Growth is bounded.** A transient buffer keeps at most `TRANSIENT_MAX_LINES`
(10 000) lines; appending past that drops lines from the top. A runaway
`make` must not eat the session.

## `:!` in a transient buffer

`:!cmd` (and `:!!`, and `:w !cmd`'s output) appends to the buffer named
`[!<cmd>]`. One job at a time, as before, so there is one such buffer at a
time; a new `:!` reuses it — clears it? No: **appends**, under a fresh
`$ <cmd>` header line, so the previous run's tail is still there to compare
against. Its name follows the latest command.

**Where it shows.** If a window already shows the buffer, output goes there
and the window stays where it is. Otherwise `:!` opens it in a horizontal
split below the current window, the way `:results` opens a Results pane —
and **the focus stays in your buffer**, as `docs/specs/shell.md` already
requires.

**The cursor follows the tail only if it was at the tail.** A window whose
cursor sits on the last line is watching the job: each appended line moves
the cursor with it. A window whose cursor was moved up is reading back
through the log, and new lines arrive below without yanking it down. This is
what a terminal's scrollback does, and it replaces the pane's
always-auto-follow.

**Lines.** `$ <cmd>` first; stdout lines as they are; stderr lines prefixed
`! `; then `exited <n>` / `killed` / `signal` as the trailer. The status line
says `! <cmd>` while it runs and `! exited <n>` / `! killed` / `! signal`
after — the pane title that used to carry this is gone with the pane.

**Stopping.** `:stop` as before. **`:bd` on the job's buffer while the job
runs stops the job, then closes the buffer** — closing the thing you were
watching a job in means you are done with it, the way closing a terminal tab
does, and it leaves no orphan. Quitting kills a running job, as before. The
Console pane's `Ctrl-C` and `x` stop keys are removed with the pane: `x` in
a buffer deletes a character, as it should.

**Editing it is fine.** It is a buffer; type in it, delete from it, `:%!sort`
it. New job output still appends at the end.

## The shape in bi

```
src/buffer.rs      Buffer::kind: Kind { File, Transient { name } };
                   Buffer::transient(name); append_lines(&[String]) with the
                   line cap; save/save_as honour the kind
src/editor.rs      transient_buffer(name) -> BufferId (find-or-create);
                   show_transient(id) (reuse a window or split below, focus
                   kept); the :! / :w ! sinks call append instead of the
                   Console; :bd on the job buffer stops the job; the nags and
                   :wa skip transient buffers; name_of shows [name]
```

The `DapConsole` pane is untouched: the debugger's `:debug console`, output
events and `:eval` keep using it. Only the shell's use of it goes.

## Tests

- `:!echo hi` creates `[!echo hi]`, shown in a split below, focus unchanged;
  it contains `$ echo hi`, `hi`, `exited 0`.
- A second `:!` appends under a new `$` line and renames the buffer.
- With the cursor on the last line, appended output moves it; with the cursor
  moved up, it stays.
- `:w` on it is refused with the copy hint; `:w /tmp/x` writes the text and
  the buffer is still transient and still `[!…]`.
- `:bd` closes it without a nag while the job is finished; while the job is
  running, `:bd` kills the job (the fake's `killed` count) and closes it.
- `:q` with a modified transient buffer does not nag.
- Appending past `TRANSIENT_MAX_LINES` drops lines from the top.
- `:%!sort` on the transient buffer works (it is a buffer).
- The debugger's Console pane still receives `output` events and `:eval`
  (regression: nothing about `DapConsole` changed).
- No LSP attach, no git baseline, no trim for a transient buffer.

## Resolved decisions

1. **Scope: `:!` only.** The debugger keeps its Console pane.
2. **`:bd` on a running job's buffer stops the job, then closes.**
3. **`:w <path>` saves a copy; the buffer stays transient.**
4. **Append, don't clear, on the next `:!`;** one buffer, renamed to the
   latest command.
5. **Cap at 10 000 lines**, trimming from the top.
