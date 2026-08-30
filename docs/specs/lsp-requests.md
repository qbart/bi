# LSP requests: definition, references, formatting

The first three request/response features over the LSP core, and the proof of
its seam: each files a typed intent, and the pump resolves it into ordinary
editor state. No new thread, no new channel, no new UI surface — a jump, a
`Results` pane, and a buffer edit are things bi already knows how to be.

Hover and completion are deliberately absent: both want a floating popup,
which bi does not have, and a popup system designed as a side effect of hover
would be a bad popup system. They wait for their own design.

## Status

**Built.** `:peek` — the definition in a split — came later and sits below
`gd`, whose machinery it borrows whole.

## The shape all three share

An editor command gathers what only the editor knows — buffer, cursor
position converted through the client's encoding, document version — and
calls `Registry::request` with a method, params, and an `Intent` variant
carrying the context the response will need (`Definition { window }`,
`References { symbol }`, `Formatting { buffer, version }`). The response
arrives through the same inbox as everything else; `accept` parses it and
returns an `Effect`; the editor applies it inside `settle`.

Refusals are statuses, not errors: no server, server not running, or a
capability the server did not claim (`definitionProvider`,
`declarationProvider`, `implementationProvider`, `referencesProvider`,
`documentFormattingProvider` — parsed truthy, since each may be a bool or an
options object) each put one line on the status bar.

A response that arrives after the world moved is the async trap, handled per
feature by what correctness needs: formatting carries the document version it
was requested at and a mismatch drops the edits — silently applying a format
computed against text that no longer exists is how a formatter eats a file.
Definition and references tolerate drift: the jump lands where the answer
said, which a moment after typing is at worst a line off.

## `gd` — definition

`gd` resolves to `:definition` (`:def`), exactly as `ga` resolves to `:alt`;
rebindable through the name `definition`. The response may be one `Location`,
a list of them, `LocationLink`s, or null — all four shapes normalise to
`(path, range)` pairs, non-`file://` URIs dropped. The first target opens
through the same path `:e` uses (a buffer already open is reused, same file
included) and the cursor lands on the range start; more than one target says
`went to the first of N`. Nothing found says so.

## `:decl`, `:impl` — the rest of the goto family

`textDocument/declaration` and `textDocument/implementation` are
`textDocument/definition` with a different method name: same params, same
four response shapes, same jump. So they are not two more features but one
`Goto` kind threaded through the machinery `gd` already built —
`lsp::Goto::{Definition, Declaration, Implementation}` picks the method, the
capability bit, and the word in every message (`no declaration found`,
`implementation: this server does not offer it`), and everything downstream
of the name is shared. A fourth member of the family would be one more
variant, not one more pipeline.

Why they exist: clangd's `definition` deliberately bounces — on a reference
it goes to the definition *if indexed, else the declaration*, and on either
end of a decl/def pair it jumps to the other — so C and C++ land in the
header when you wanted the source often enough to want the explicit ask.
`:decl` is the header's side of the question, `:impl` the bodies — which for
Rust and friends also means trait implementations and overrides.

`:impl` wears `gi`, completing the family the way `gd` and `gr` began it —
the jump's own initial, on a key vim spends re-entering insert mode, which
bi does not do. `:decl` stays a command only: it is the occasional precise
question, and `:declaration` and `:implementation` are the long spellings.

## `:peek` — the definition beside you

`gd` replaces what you were looking at, and half the time the question was
"what *is* that" rather than "take me there". `:peek` answers it without the
round trip: a vertical split opens, the definition request runs in it, and
the answer lands there — the definition on one side, the call site untouched
on the other, focus on the definition so `Ctrl-W q` is the whole way back.

The implementation is the composition it sounds like: check the server offers
definitions *first* — a `:peek` with nothing to show must not leave an empty
split behind — then `:vs`, then the same `:definition` the `gd` key runs,
which lands in the focused window and the focused window is now the split.
The async plumbing, the response shapes and the buffer reuse are all `gd`'s,
untouched; a `:peek` whose answer is "nothing found" reports it on the status
line and leaves the split showing the same file, which is what `:vs` would
have shown and is yours to close.

No default key. `gp` would be the vim-adjacent spelling, but the `g` row is
filling up and a command you run a few times an hour is cheap to type;
`"<leader>p" = ":peek<CR>"` is one line of config.

Tests: `:peek` with no server says why and does not split.

## `gr` — references

`gr` resolves to `:references` (`:refs`); the name is `references`. The
targets become a `Results` pane — the pane whose spec already promised "LSP
references want this same list" — rooted at the client's workspace root,
sorted by file then position, each row's text read from the open buffer when
there is one (unsaved edits included) and from disk otherwise. The intent
carries the symbol under the cursor at request time, which becomes the
pane's title and its query — so `:replace` over a references pane rewrites
exactly the occurrences the server named, which is rename spelled with two
commands bi already has.

## `:format` — whole-file formatting

This is `:format`'s server half — a buffer whose filetype has an external
formatter never gets here, because the tool claims the command first. See
`docs/specs/fmt.md` for that layer and the order's why.

`:format` (`:fmt`) sends `textDocument/formatting` with the options in force
(`tab_width`, `expandtab`) — the options, not the `.editorconfig`, for the
same reason `:retab` reads them. The response is a list of `TextEdit`s;
they convert to char ranges against the current rope, apply in reverse order
so earlier edits cannot move later ones, and close as **one undo step** — a
whole-file reformat that `u` cannot take back in one keystroke is a trap.
The cursor maps through the edits like every unfocused window's selections
do. Not on save, and not a keybinding: reformatting a file is a decision, and
`:w` making whole-file diffs behind you is the `:retab` argument again.
