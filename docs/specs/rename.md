# Rename

`:rename <newname>` — the symbol under the cursor, renamed across the
project by the server. The verb finally means what an LSP client should
mean by it: `:mv` moves files, `:rename` renames *names*.

## The command

`:rename` takes exactly one argument, the new name, and that is the whole
interface: no prompt machinery, no float — the argument is the name, the
way `:mv`'s arguments are the paths. An empty `:rename` is an error saying
what it takes. The cursor is the position asked about; visual mode adds
nothing, because a rename is of the symbol, not of a range.

The request is `textDocument/rename` with the position and `newName`. The
answer is a `WorkspaceEdit` — the same shape code actions apply — and it
goes through the same applier with the same guarantees: converted against
the current rope, bottom-up, one undo step per buffer; files not open are
opened and left as modified buffers to inspect before `:w`; versioned
entries are gated, and the buffer the rename was *requested* on is gated
against its version at request time; resource operations apply in order —
which is what lets rust-analyzer rename a module by renaming its file. A
`null` answer is "nothing at the cursor here", as a status.

## What this is not

No `textDocument/prepareRename` round-trip — the server validates the
rename when asked to do it, and an error comes back as a status either
way; asking twice buys a placeholder bi has no prompt to show. No default
keybinding: `:rename` is a command like `:mv` is, and a key that wants it
can be bound to it.

## Capabilities

The client declares `textDocument.rename` (presence, nothing more — no
`prepareSupport`). The server-side gate is `renameProvider`, parsed truthy
like every other provider, refused by name when absent.

## Testing

Through the fake server: `:rename counter` sends the position and the new
name; a multi-file `WorkspaceEdit` answer lands in both buffers with a
status; a `null` answer is a status; a bare `:rename` and a server without
`renameProvider` are refused before sending.
