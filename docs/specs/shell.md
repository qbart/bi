# Shell commands

`:!cmd` runs a command and shows what it printed. `:{range}!cmd` filters lines
through one. `:r !cmd` reads a command's output in; `:w !cmd` writes lines
out to one. Vim's four spellings, with one deliberate split underneath them.

## Status

**Built.** Decisions resolved with the user on 2026-09-06; see the end.

## What vim does, and what bi cannot

Vim's `:!cmd` hands the *terminal* to the shell: vim suspends, the command
owns stdin and stdout, `Ctrl-C` is the process's own SIGINT, and vim redraws
when you press Enter. Nothing in vim is running while the command is. A
filter (`:%!sort`) is the same blocking shape over pipes, and `:r !`/`:w !`
are the two halves of it.

bi has no pty (the debugger spec punted `runInTerminal` for the same reason),
and its main loop must keep drawing — every async source joins by sending
into it (`Wake::Lsp`, `Wake::Dap`). So bi cannot suspend itself around a
command, and the four spellings split into two shapes by what they are *for*:

- **Filters** — `:{range}!cmd`, `:r !cmd`, `:w !cmd` — are **synchronous**.
  The buffer edit *is* the result, and an edit made in the meantime would
  race the text the filter was computed against. They run through the
  process runner formatters already use (`fmt::Run` / `ProcessRun`), with
  its **5 s guard**: a filter that needs longer is not a filter, it is a job.
- **Bare `:!cmd`** — `:!cargo test`, `:!make` — is a **job**. It runs in the
  background, its output streams into a buffer as it arrives, and the editor
  stays live. A job has no timeout; it has `:stop`.

## Jobs

**One at a time.** A second `:!` while one runs is refused with a status:
`! is running <cmd> — :stop it first`. One job is legible; a list of them is
a feature nobody asked for.

**Output goes to a transient buffer** (`docs/specs/transient.md`), named
`[!<cmd>]` and renamed to the latest command each time `:!` runs. `:!` opens
it in a horizontal split below the current window if no window shows it
already, and appends `$ <cmd>` as the first line so a run is delimited from
the previous run above it — the buffer keeps at most `TRANSIENT_MAX_LINES`
(10 000) lines. The status line carries the verdict that used to live on a
pane title: `! <cmd>` while the job runs, then `! exited <n>`, `! killed` or
`! signal` once it ends — there is no pane title any more, because there is
no pane, only a buffer any window can show or not.

**stdin is closed at spawn.** A program that prompts gets EOF, not a hang.
That is the honest answer without a pty: interactive programs are what the
terminal is for.

**stderr is interleaved with stdout** in arrival order, each line prefixed
`! ` the way debuggee stderr already is — a build's errors belong beside its
progress, not in a second buffer.

**Escape sequences and other control characters are stripped before a line
reaches the buffer**, and a carriage return rewinds it — the same rule as
any job's output (`docs/specs/ansi.md`) — so a coloured build log reads as
plain text and a progress bar that redraws itself keeps only its final
state.

**Exit:** the status line says `! exited 0` (or `1`, or `killed`), and the
trailer line lands in the buffer. Non-UTF-8 output is decoded lossily.

**Stopping.** `:stop` sends SIGTERM, waits ~2 s, then SIGKILL, in the order
the debugger's `end_session` already uses. The waiting happens on a thread of
its own: `:stop` returns to the editor the moment the SIGTERM is away, or a
job that ignores it would freeze the session for the whole grace period. The
signals go to the job's **process group**, not to the `sh` at its head —
`sh -c` only execs a *simple* command, so `:!cargo build; ./run` leaves a
shell with a child underneath it, and signalling the leader alone would kill
the shell and orphan the build. The exit is filed only once the process is
confirmed gone, so the job the editor still holds is a job that is still
alive, and quitting can always take it with it. One consequence worth
knowing: because the job has its own process group, a `Ctrl-C` typed at the
terminal reaches bi and not the job — `:stop` is the way to stop it, not a
key. **`:bd` on the job's buffer while it runs stops the job first, then
closes the buffer** — closing the thing you were watching a job in means you
are done with it, the way closing a terminal tab does, and it leaves no
orphan (`docs/specs/transient.md` §"Stopping"). Quitting with a job running
kills it on the way out (`shutdown_shell`, beside `shutdown_lsp` and
`shutdown_dap`); nothing is orphaned.

## Filters

`:{range}!cmd` replaces the range's lines with the command's stdout, as one
edit (one undo). No range means the current line, as in vim. The command's
stdin is the lines, newline-terminated; on a non-zero exit the buffer is
left alone and the status shows the first stderr line — the formatter's
rule, for the formatter's reason: a filter that failed has nothing to say
about your text.

The runner drains the command's stdout and stderr while it waits, so an
answer bigger than a pipe buffer (~64 KB) is fine: `:%!sort` on a file of
any size is a filter, not a hang. `:r !cmd` with no output at all makes no
edit, and so leaves no undo step.

`:[range]r !cmd` inserts stdout below the cursor line (or below the range's
last line). `:[range]w !cmd` feeds the lines (default: whole buffer) to the
command's stdin and shows its output in the same transient buffer as a job's
— but synchronously, under the guard, and the buffer is not marked saved.

`:!!` repeats the last `:!` job; a `!` inside a filter or job command line is
**not** expanded to the previous command (vim's rule, dropped: `:!grep '!'`
should mean what it says). `%` expands to the current file's path and `#` to
the alternate's; `\%` and `\#` are literal.

## The shape in bi

```
src/shell.rs        Job (pure state: cmd, lines seen, exit), the Spawn seam,
                    ProcessSpawn (the thread that pumps output into an Inbox-
                    shaped slot), and the pure `expand` for %/#      (core)
src/editor.rs       :! / :{range}! / :r ! / :w ! / :!! / :stop parsing and
                    dispatch; `pump_shell` in settle appends into the job's
                    transient buffer (`docs/specs/transient.md`); `:bd` on
                    that buffer stops the job
src/main.rs         Wake::Shell; the waker; ProcessSpawn registered
```

The lib boundary keeps its rule: the editor never spawns. `set_shell_spawner`
/ `set_shell_waker` cross it the way `set_dap_spawner` / `set_dap_waker` do,
so a headless embedder or a test supplies a fake and no process ever starts.
The reader thread does nothing but push lines into the job's slot and ring
the waker; `pump_shell` in `settle` turns them into lines appended to the
job's transient buffer, the way `pump_install` turns an install's result
into state.

Filters do not need any of that: they call the fmt runner and apply the
result inside the command, like `:format`.

## Tests

- `:!echo hi` opens `[!echo hi]` in a split below (or reuses a window
  already showing it), shows `$ echo hi`, `hi`, and `exited 0`; the status
  says so.
- A second `:!` while one runs is refused and names the running command.
- `:stop` kills a job that does not exit on SIGTERM; the status says killed.
- `:bd` on the job's buffer while it runs stops the job, then closes the
  buffer.
- Quit with a job running kills it.
- `:%!sort` replaces the buffer's lines with sorted ones, as one undo step; a
  failing filter leaves the buffer untouched and reports the first stderr
  line.
- `:r !echo x` inserts `x` below the cursor; `:w !cat` shows the buffer's
  lines in the transient buffer and does not clear the modified flag.
- `%` and `#` expand; `\%` does not; `:!!` repeats.
- A prompting command (`read x`) exits on EOF rather than hanging.
- With a fake spawner nothing is spawned and the whole flow still works.
- A job that prints colours shows plain text; `:r !` keeps the escapes
  verbatim.

## Resolved decisions

1. **Two shapes:** filters synchronous under the 5 s guard; bare `:!` a
   background job with no timeout.
2. ~~**Console reuse:** job output goes to the debugger's Console pane (the
   user's explicit choice), not a new pane.~~ Superseded — see
   `docs/specs/transient.md` and the Deviations below.
3. **One job at a time;** `:stop` or `:bd` on its buffer to end it; killed
   on quit.
4. **stdin closed;** no pty; no interactive commands in v1.
5. **`!` in a command line is literal** (vim expands it to the previous
   command); `:!!` is the repeat.

## Deviations from the design

Six places where the build settled a question this spec's text left
implicit, each recorded here so nobody re-derives it from scratch:

1. **The grammar ruling: bare `:!cmd` is the job; the current-line filter is
   `:.!cmd`.** This is vim's real grammar, not a bi invention — `:!` with no
   range is `:!{cmd}`, the shell form; a range of *any* kind, `.` included,
   is what makes a `!` command a filter. A scope-less `:!` is therefore never
   a filter, even for "just the current line" — you write `:.!cmd` for that,
   the same as vim.
2. **`Read`/`Write` report `read N lines` / `wrote N lines`.** Wording
   chosen at build time to match the buffer's own "N lines" phrasing
   elsewhere in the status line; nothing in the spec's text mandated it.
3. **A job the editor kills reports `killed`; one that died of a signal
   nobody asked for reports `signal`.** The spec's text names `exited <n>`
   and `killed` only, but a job can also end on a SIGSEGV or a SIGHUP that
   bi had nothing to do with, and there is no exit code to print for it.
   `signal` is that third trailer — in the buffer's own line and in the
   status (`! signal`) alike. Which of the two a death gets is a question of
   *who asked*, not of what the kernel did: `:stop` marks the job before it
   signals it, so even a job that handles SIGTERM and exits 0 is reported
   `killed`.
4. **`:!` moves focus into the log.** The transient buffer gets the output
   and you land in it — a job that opens a program and closes it leaves you
   where its output is, not two windows away (the user's call, 2026-09-08,
   reversing the first build, which kept focus in your buffer the way vim
   leaves you in place). `%` and `#` are expanded before focus moves, and
   from inside the log `%` resolves through the log window's alternate — the
   file it was opened over — so `:!!` from the log repeats the same command.
   `Ctrl-W p` goes back to the window the command was run from.
5. **One "no runner" status, shared by jobs and filters.** A headless
   embedder that supplies no spawner (`set_shell_spawner`) sees the same
   `! : this frontend supplies no runner` whether `:!cmd`, `:{range}!cmd`,
   `:r !cmd` or `:w !cmd` triggered it — there was no reason for the two
   shapes to word a missing seam differently.
6. **Console reuse is reversed: `:!` output goes to a transient buffer, not
   the debugger's Console pane.** Resolved decision 2 above was the user's
   explicit call at the time this spec was written, before
   `docs/specs/transient.md` existed to make the alternative concrete. A
   pane is a window *content kind*, like the file tree: it cannot be
   `:bd`-closed, edited, or yanked from, and its stop keys are its own,
   separate from every other key in the editor. Build log text is text, and
   text belongs in a buffer — one you can read, search, filter with
   `:%!sort`, switch to with `Ctrl-^`, and close with `:bd` like any other.
   The debugger's own Console is untouched; only `:!`'s use of it moved.
