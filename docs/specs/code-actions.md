# Code actions

The last of the everyday LSP requests: "what would the server do about
*this*?" — the quickfix under a diagnostic, the import a name is missing,
the refactor a selection allows. vim spells it `<leader>ca` by convention;
bi gives it `:actions`, and `<leader><leader>` out of the box.

## The keys

`:actions` is the command, in the family `:definition`, `:references` and
`:format` already form — a request is a command first and a key second.

`<leader><leader>` is the default binding, and the first leader binding bi
ships. The principle that the leader has no built-in meaning bends here
deliberately: an action menu is the one thing every editor with an LSP
client puts within two keystrokes, and a fresh bi with a working server
should have it without a config file. It stays *yours* all the same: the
default is installed only where you have not spoken —
`"<leader><leader>" = ":actions<CR>"` is what it means, rebinding it wins,
`"<leader><leader>" = false` removes it, and it follows whatever `leader`
you set, because it is installed against the live leader when the keymap
is, not spelled into a file against the shipped one.

In visual mode the selection is the range the server is asked about;
otherwise the cursor is, as a collapsed range. Either way the request
carries the stored diagnostics that overlap it — kept in the wire shape
the server sent them in, `code` and `data` included, because clangd only
offers the fix for a diagnostic it recognises as its own.

## The answer is a picker

Actions come back as a list of titles; titles are what a picker is for.
One new `PickerKind`, rows in the server's order (the order is the
server's ranking), disabled actions dropped rather than greyed — bi has
no grey, and a row that cannot be chosen is a row that should not be
offered. No actions is a status line, not an empty picker.

Choosing a row does what the action says:

- **An edit** (`WorkspaceEdit`) is applied — see below.
- **A command** goes back as `workspace/executeCommand`; whatever edits it
  causes arrive as a server→client `workspace/applyEdit`, which bi now
  honours (it used to answer `applied: false`) and applies through the
  same path.
- Both, in that order, when the action carries both.
- **Neither** means the action is a claim ticket: the server offered a
  title and kept the expensive part — computing the edit — for later.
  The chosen action goes back verbatim as `codeAction/resolve` (the
  `data` field is the server's bookmark, carried untouched, which is why
  actions keep their raw wire shape beside the parsed one), and the
  filled-in answer runs through the same edit-then-command path. Only
  the chosen action is resolved, only when it needs it: the menu stays
  as cheap as the server can make it. A resolved action that still has
  neither half is a status, not a silence.

## Applying a workspace edit

A `WorkspaceEdit` is `changes` (uri → edits) or `documentChanges`
(versioned document edits); both are applied the way `:format` applies its
edits — converted against the current rope, bottom-up so an earlier edit
cannot move a later one, one undo step per buffer. Files not open are
opened through the same path `:e` uses and left as modified buffers: an
edit you have not saved is an edit you can still inspect and undo, which
is how vim's clients behave too.

Staleness is gated where a version exists to gate with, exactly as
`:format`: a `documentChanges` entry naming a version that is not the
buffer's current one drops that entry with a status; the buffer the action
was *requested* on is gated against the version at request time. Unversioned
`changes` to other files are taken at their word — the protocol offers
nothing better.

Resource operations — create/rename/delete file entries inside
`documentChanges` — are applied in the order the server listed them,
interleaved with the text edits, because the order is the meaning: a
"move module to file" creates the file, then fills it, then empties the
old one. They are filesystem operations and land on the filesystem, the
way `:create`, `:mv` and `:delete` land — text edits stay in unsaved
buffers you can inspect, but a created file exists, a renamed file has
moved (any open buffer follows it, its syntax re-picked and its LSP
document re-attached under the new name), and a deleted file is gone
(its buffer, if open, stays — text and history intact, as `:delete`
already behaves). `overwrite` and `ignoreIfExists` options are honoured;
`recursive` likewise for delete.

The failure rule is *abort*: the first operation that fails stops the
whole edit there, with a status naming what failed and what had already
been done — applying half of a rename silently is the one outcome worse
than either whole. This is also what bi declares as `failureHandling`.

## Capabilities

`initialize` now declares `codeActionLiteralSupport` (with the standard
kind set) so servers send `CodeAction` literals rather than bare commands
— and `resolveSupport` for `edit` and `command`, which invites servers to
keep the expensive halves lazy and lets the menu open fast; bi cashes the
ticket with `codeAction/resolve` on accept. `workspace.applyEdit` is
declared true, which is what makes command-backed actions land, and
`workspace.workspaceEdit` declares `documentChanges` plus the three
`resourceOperations` with `failureHandling: "abort"` — a server only
sends what the client admits to understanding, and before this
declaration a rename-file refactor was never even offered. The
server-side gate is `codeActionProvider`, parsed truthy like every other
provider.

## What this is not

No auto-applied `source.fixAll` on save, no lightbulb in the gutter, no
kind filtering. The menu, chosen by hand, is the feature.

## Testing

Through the fake server: `:actions` sends the request with the selection's
range and the overlapping diagnostics echoed back; a response opens the
picker and choosing applies a multi-edit `WorkspaceEdit` as one undo step;
a versioned document edit against moved text is dropped; a command-only
action sends `executeCommand`, and the server's `workspace/applyEdit` is
applied and answered `applied: true`; a response with no actions is a
status. An action with neither edit nor command sends `codeAction/resolve`
with `data` intact and applies the answer. A `documentChanges` list that
creates a file, edits it, renames another and deletes a third does all
four in order — the created file exists and holds its edits, the renamed
buffer follows its file, the deleted file is gone — and one that renames
a file that is not there stops at the failure with nothing after it
applied. The keymap: `<leader><leader>` runs `:actions` out of the box,
a user binding of the same sequence wins, and `= false` removes it.
