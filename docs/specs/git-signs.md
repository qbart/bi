# Git signs

The minimum of git an editor owes you while you type: which lines differ from
what git has, and by how much. A mark per changed line in the gutter, and a
numstat in the window's status row. Deliberately nothing more — no hunk
navigation, no staging, no blame, no diff view. Those are git's own tools;
this is the editor answering "what have I touched" without leaving the file.

## Status

**Built.**

## The baseline

A line is added, changed or removed *relative to something*, and that
something is the index — `git show :0:<file>` — not `HEAD`. The index is what
a commit would take as-is, so the signs read "what a commit would still be
missing", which is the question being asked while editing. For the common case
of nothing staged the two are the same file.

The core never runs git itself. [`Editor::set_git_baseline`] takes a loader —
`&Path -> Option<String>` — the same shape as `set_lsp_spawner`: the library
stays embeddable and process-free, the frontend hands in
[`crate::git::baseline`], which shells out `git show` in the file's directory,
and a test hands in a closure returning a fixed string. No loader, no signs —
which is also why every existing test is untouched by the feature existing.

Every failure is silence, not an error: no repository, an untracked file, git
not installed — there is nothing to say about a file git holds no copy of, and
a gutter that nagged about it would be noise. The baseline is fetched when a
buffer is opened, reverted, or written — the moments the file's relationship
to the repository can have moved — and never per keystroke.

## The diff

`imara-diff` (histogram), buffer text against baseline, recomputed at the
drain in `settle` whenever the buffer's edit counter has moved — the same
policy as the parse tree: the signs follow the text within the keystroke, and
an untouched buffer costs nothing. Each diff hunk becomes signs:

- lines only in the buffer — **added**, `▎` per line in `git_add`
- lines replaced — **changed**, `▎` per line in `git_change`; a replacement
  of unequal size counts the overlap as changed and the excess as
  added/removed
- lines only in the baseline — **removed**, `▁` on the row the deletion sits
  under, in `git_delete`; `‾` on row zero when the file's first lines are gone

A row that is itself added or changed and also has a deletion under it wears
the add/change sign — the mark about text that exists beats the mark about
text that does not.

## One cell, two tenants

The gutter cell (`gutter = 1`) was reserved "for a git sign, a diagnostic, a
breakpoint". It now has two clients, and the diagnostic wins the cell: it is
the rarer mark and the one that says something is *wrong*, while a git sign
merely says something is *different* — usually true of half the screen. The
merge happens in `Editor::gutter_signs`, so a frontend still paints one list.

## The numstat

`+3 ~1 -2` in the focused window's status row, to the left of the mode: added,
changed, removed line counts from the same diff, each part in its sign's
colour and absent at zero — a clean file's status row is exactly what it was.
The core exposes [`Editor::git_stats`]; the composition is the frontend's,
like the rest of the status row.

## Options and theme

`git_signs = true` in `[options]` gates the drawing — the gutter marks and the
numstat both — never the computing, exactly as `diagnostics` gates its three
marks. Per-filetype and `:set` come free. Three theme keys: `git_add`,
`git_change`, `git_delete`.

## Deliberately not here

Hunk motions (`]c`), hunk text objects, staging, unstaging, blame, a diff
split, watching `.git` for changes mid-session. `:e` re-reads the baseline;
that is the whole of the refresh story.
