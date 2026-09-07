# Shell commands

`:!cmd` runs a command and shows what it printed. `:{range}!cmd` filters lines
through one. `:r !cmd` reads a command's output in; `:w !cmd` writes lines
out to one. Vim's four spellings, with one deliberate split underneath them.

## Status

**Approved for build.** Decisions resolved with the user on 2026-09-06; see
the end.

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
  background, its output streams into a pane as it arrives, and the editor
  stays live. A job has no timeout; it has `:stop`.

## Jobs

**One at a time.** A second `:!` while one runs is refused with a status:
`! is running <cmd> — :stop it first`. One job is legible; a list of them is
a feature nobody asked for.

**Output goes to the debugger's Console pane** (`docs/specs/debug.md`), reused
whole: the same `DapConsole`, the same 2000-line bound, the same renderer,
the same keys. `:!` opens it if no window shows one, exactly as
`:debug console` does, and appends `$ <cmd>` as the first line so a run is
delimited from the debuggee output or the previous run above it. The pane's
title reads `! <cmd>` while the job runs, then `! <cmd> — exited <n>` or
`! <cmd> — killed`.

**stdin is closed at spawn.** A program that prompts gets EOF, not a hang.
That is the honest answer without a pty: interactive programs are what the
terminal is for.

**stderr is interleaved with stdout** in arrival order, each line prefixed
`! ` the way debuggee stderr already is — a build's errors belong beside its
progress, not in a second pane.

**Exit:** the status line says `! exited 0` (or `1`, or `killed`), and the
trailer line lands in the console. Non-UTF-8 output is decoded lossily.

**Stopping.** `:stop` — and `Ctrl-C` or `x` *in the Console pane* — sends
SIGTERM, waits ~2 s, then SIGKILL, in the order the debugger's
`end_session` already uses. `Ctrl-C` is bound only in the pane: in Normal
mode it keeps meaning what it means. Quitting with a job running kills it on
the way out (`shutdown_shell`, beside `shutdown_lsp` and `shutdown_dap`);
nothing is orphaned.

## Filters

`:{range}!cmd` replaces the range's lines with the command's stdout, as one
edit (one undo). No range means the current line, as in vim. The command's
stdin is the lines, newline-terminated; on a non-zero exit the buffer is
left alone and the status shows the first stderr line — the formatter's
rule, for the formatter's reason: a filter that failed has nothing to say
about your text.

`:[range]r !cmd` inserts stdout below the cursor line (or below the range's
last line). `:[range]w !cmd` feeds the lines (default: whole buffer) to the
command's stdin and shows its output in the Console like a job's — but
synchronously, under the guard, and the buffer is not marked saved.

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
                    dispatch; `pump_shell` in settle; Console reuse
src/main.rs         Wake::Shell; the waker; ProcessSpawn registered
src/input.rs        Ctrl-C and x in the Console pane → ShellCmd::Stop
```

The lib boundary keeps its rule: the editor never spawns. `set_shell_spawner`
/ `set_shell_waker` cross it the way `set_dap_spawner` / `set_dap_waker` do,
so a headless embedder or a test supplies a fake and no process ever starts.
The reader thread does nothing but push lines into the job's slot and ring
the waker; `pump_shell` in `settle` turns them into console lines, the way
`pump_install` turns an install's result into state.

Filters do not need any of that: they call the fmt runner and apply the
result inside the command, like `:format`.

## Tests

- `:!echo hi` opens the Console (or reuses it), shows `$ echo hi`, `hi`, and
  `exited 0`; the status says so.
- A second `:!` while one runs is refused and names the running command.
- `:stop` kills a job that does not exit on SIGTERM; the title says killed.
- Quit with a job running kills it.
- `:%!sort` replaces the buffer's lines with sorted ones, as one undo step; a
  failing filter leaves the buffer untouched and reports the first stderr
  line.
- `:r !echo x` inserts `x` below the cursor; `:w !cat` shows the buffer in
  the Console and does not clear the modified flag.
- `%` and `#` expand; `\%` does not; `:!!` repeats.
- The Console's `Ctrl-C` stops a job; Normal-mode `Ctrl-C` is unchanged.
- A prompting command (`read x`) exits on EOF rather than hanging.
- With a fake spawner nothing is spawned and the whole flow still works.

## Resolved decisions

1. **Two shapes:** filters synchronous under the 5 s guard; bare `:!` a
   background job with no timeout.
2. **Console reuse:** job output goes to the debugger's Console pane (the
   user's explicit choice), not a new pane.
3. **One job at a time;** `:stop` / pane `Ctrl-C` / pane `x` to end it;
   killed on quit.
4. **stdin closed;** no pty; no interactive commands in v1.
5. **`!` in a command line is literal** (vim expands it to the previous
   command); `:!!` is the repeat.
