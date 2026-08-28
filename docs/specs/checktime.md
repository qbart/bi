# Checktime

What happens when the file changes on disk while a buffer holds it. Until now:
nothing. The buffer knew its path and not one fact about what it read from it,
so a `git checkout`, a formatter, or an `rm` in another pane went unnoticed
until `:w` silently clobbered it or `:e` was typed on faith. This spec gives
the buffer a memory of the disk and a policy for disagreeing with it — vim's
`checktime` model, expressed in bi's idioms.

## The snapshot

At every read and every write, the buffer records what the file looked like on
disk: mtime and size. That pair is the whole of the machinery — no filesystem
watcher, no thread, no dependency. A check is one `stat` compared against the
snapshot, which makes it cheap enough to run at every moment that matters and
portable to any frontend for free, since it lives entirely in the core.

A watcher (`notify`) could sit on top later, injected the way `set_git_baseline`
is, waking the loop instead of waiting for a poll. Deliberately not built now:
the moments below already catch everything within a couple of seconds, and a
watcher buys latency at the price of a dependency and per-frontend wiring.

## The moments

A buffer is checked:

- when a window is pointed at it — the buffer-switch moment, in `show`, the
  same place the cursor is restored;
- when the terminal regains focus — the TUI enables focus events and calls
  [`Editor::focus_gained`], which checks every buffer; an embedder that never
  calls it simply loses this moment;
- on a timer — every `checktime` milliseconds (default 2000, 0 turns it off)
  the visible buffers are checked, through the same `redraw_in` clock the yank
  flash uses, so the frontend's loop needs nothing new;
- on demand — `:checktime` checks every buffer in the session;
- before every write — the guard below, which runs even with everything else
  turned off.

## The policy

What a check finds decides what happens, and the deciding fact is whether the
buffer is modified — whether there are two sets of changes or one:

- **Disk changed, buffer clean.** With `autoread` (default on): the buffer
  reloads silently, status `"x.go" reloaded (u undoes it)`. With it off:
  status `"x.go" changed on disk (:e reloads)` and nothing moves.
- **Disk changed, buffer modified.** Never autoread — both sides hold edits
  and the editor must not pick a winner. Status: `"x.go" changed on disk and
  in the buffer (:e! loads disk, :w! keeps yours)`.
- **File deleted.** The buffer and its text stay, whatever its modified state:
  the buffer is now the only copy and bi never recreates the file on its own.
  Status: `"x.go" no longer on disk (:w recreates it)`. A plain `:w` writes
  without complaint — recreating is the fix, not a conflict.
- **File recreated after a deletion** is a change, and takes the change rows
  above.

Each disagreement is reported once. The buffer remembers which on-disk state
it warned about and stays quiet until the disk moves again — a warning per
change, never a nag per poll. There is no prompt: the editor has no prompt
machinery and gains none here. The status line names the state and the
commands that resolve it, and the `!` is the assent, exactly as everywhere
else.

## The write guard

`Buffer::save` stats the file before truncating it. If the disk no longer
matches the snapshot — someone else wrote since we read — the save refuses:
`"x.go" changed on disk since last read (:e loads it, :w! overwrites)`. The
disk is untouched, precisely as a failed encoding already leaves it. `:w!`,
`:wa!`, `:wq!` and `:x!` force the write. A missing file is not a conflict —
that write recreates it silently.

This guard is the floor under everything above: with `autoread` off, the poll
off, and a terminal that reports no focus, the one thing that can still never
happen is bi overwriting work it has not seen.

## Reload keeps the undo history

Reload — autoread's and `:e`'s both — no longer rebuilds the buffer. It reads
the new content, diffs it against the rope (`imara-diff`, the git-signs
machinery), and applies the hunks through the ordinary edit funnel as one
revision. Consequences, each the point:

- **`u` undoes a reload**, autoread's included — and `:e!` over a modified
  buffer now discards nothing irrecoverably, where it used to throw the text
  and its history away together.
- The parse tree and the language servers receive the reload as the
  incremental edits it is, through the same drain as typing — no rebuild, and
  the LSP document finally stays in sync across a reload.
- Cursors in every window ride through `Edit::map` instead of being clamped,
  so a reload that touches lines 1–3 does not move a cursor on line 400.

After the diff is applied the revision is marked saved: a freshly reloaded
buffer is unmodified, and undoing past the reload makes it modified again,
which is exactly what it then is.

## Options

- `autoread = true` — whether a clean buffer follows the disk silently.
- `checktime = 2000` — the poll interval in milliseconds; 0 is no polling,
  which leaves the switch, focus, `:checktime` and write-guard moments.

Both in `[options]`, per-filetype and `:set` for free like every option.

## Where it lives

- `src/buffer.rs` — the `DiskState` snapshot (mtime, size), captured in
  `open_how` and `save`; the save guard; `reload` rewritten as diff-and-apply;
  `check_disk` answering clean/changed/deleted against a fresh stat.
- `src/editor.rs` — the policy: `check_buffer` applying the matrix above,
  called from `show`, `focus_gained`, the `redraw_in` clock, `:checktime`;
  `ExLine::Write`/`WriteAll`/`WriteQuit` carrying the `!`.
- `src/main.rs` — `EnableFocusChange` beside bracketed paste, and the
  `FocusGained` event forwarded to the editor. The whole of the frontend's
  share.

## Tests

- A clean buffer whose file is rewritten externally reloads on `:checktime`
  and one `u` brings the old text back.
- With `autoread` off the same situation only warns, and the text stays.
- A modified buffer whose file is rewritten externally warns once, does not
  reload, and does not repeat the warning on the next check.
- `:w` over an externally-rewritten file refuses and leaves the disk alone;
  `:w!` writes; a `:w` after `:e` writes without force.
- Deleting the file under a buffer warns once, keeps the text, and `:w`
  recreates the file without complaint.
- Reload maps a cursor below the changed lines through the edit instead of
  clamping it.
- `:e!` over a modified buffer loads the disk text and `u` restores the
  discarded changes.
- Saving refreshes the snapshot: a save followed by a check finds nothing.
