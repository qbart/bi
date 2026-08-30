# External formatters

`:format` has so far meant `textDocument/formatting` — ask the server, apply
its edits (`docs/specs/lsp-requests.md`). That covers rust-analyzer and
clangd, and leaves out every language whose server does not format: C3's
servers offer navigation and completion but no formatting, while the official
toolchain ships `c3fmt`, a stdin→stdout filter. The command the user already
has is the answer; bi just has to run it.

## Status

**Built.**

## The config

```toml
[fmt.tools.c3fmt]
command   = ["c3fmt", "--stdin", "--stdout"]
filetypes = ["c3"]
```

The shape is `[lsp.servers.<name>]`'s on purpose: `command` is the argv,
`filetypes` the buffers it claims, `enabled = false` inside a section turns
that tool off, and a user's section patches the same-named built-in
field-wise. The same refusal applies too: **`command` is not read from a
project config** — a repository does not get to name the binary bi runs.

There is no `[fmt] enabled` master switch. A tool table you can empty out
per-tool does not need a second way to be empty, and `:fmt` without any tool
still means something — the server.

## What `:fmt` does now

The first enabled tool claiming the buffer's filetype wins; with none, the
request goes to the language server exactly as before. Tool first, not server
first: where a dedicated formatter exists it is the project's source of truth
— gofmt, not gopls's opinion of gofmt — and where none is configured nothing
changed.

The contract is the classic filter: the whole buffer on stdin, the whole
formatted file on stdout, exit 0. Anything else — nonzero exit, a tool that
could not be run at all — leaves the buffer untouched and puts the first line
of stderr (or the spawn error) in the status. A formatter that prints a
diagnostic *and* exits 0 is taken at its word and its stdout applied; that is
the contract, and tools that follow it (c3fmt, gofmt, rustfmt, clang-format)
mean it.

The tool runs with its **cwd at the file's directory** (the editor's own cwd
for a pathless buffer), which is how filters that walk up looking for their
config — `.c3fmt`, `.rustfmt.toml` — find it without bi passing filenames it
may not have.

## Synchronous, and why that is right

The run blocks the editor until the tool answers, with a five-second guard
that kills a tool that hangs. The LSP path is asynchronous and pays for it
with a version gate — "text changed under `:format`" — because the answer is
computed against text that may be gone by the time it lands. The filter run
has no such race to guard: the text cannot change under a call that blocks,
so the result always fits, every time. Formatters are fast; a whole-file
reformat that takes visible time is broken, and the guard turns broken into
a status line instead of a hung editor.

## Applying the answer

The output replaces the buffer as **one edit and one undo step**, trimmed to
the changed middle — the longest common prefix and suffix stay put, so the
edit is the difference, not the file. That is what keeps the cursor honest:
selections map through the one edit like they do through every LSP edit, and
a cursor above or below the changed region does not move at all. Identical
output is "already formatted" and no undo entry.

## The runner is injected

The core does not spawn processes; the frontend hands it a runner
(`fmt::Run`), the same shape as `lsp::transport::Spawn` — `main.rs` installs
the real one, tests a fake. An embedder that supplies none gets a status
line naming the tool it could not run, not a crash and not a silent detour
to the server — a tool was configured, and pretending otherwise would format
the file two different ways depending on a host detail no config mentions.

## The format row in `:actions`

A formatter language always has a menu to open: `:actions`
(`<leader><leader>`) leads its picker with a preselected `format — <tool>`
row, the server's own offers after it, and opens with that one row even
when the server answered nothing or there is no server at all. Reformatting
is the ask the menu can always answer for these buffers, and it should be
an accept away. Languages without a tool are untouched — their menu is the
server's answer and nothing else. See `docs/specs/code-actions.md`.

## C3, while we are here

The built-ins gain C3 alongside the formatter: `[lsp.servers.c3-lsp]`,
`command = ["c3-lsp"]`, filetypes `["c3"]`, rooted at `project.json`. The
server is the user's own build (tonis2/lsp, compiled with `c3c build lsp`);
bi only names the binary, and a config section renames it like any other.

## Tests

- `:fmt` on a C3 buffer runs the tool, the buffer becomes its stdout, one
  undo step puts it back.
- Identical output says "already formatted" and writes no undo entry.
- A nonzero exit leaves the buffer alone and surfaces stderr's first line.
- No runner installed: the tool is named in the status, nothing is touched.
- A filetype no tool claims still formats through the server, gated by
  version, as `lsp-requests.md` always said.
- `[fmt.tools.c3fmt] command` in a project-local config is refused with the
  same sentence the server table uses.
- A user section patching only `command` keeps the built-in filetypes.
- `:actions` on a formatter language opens with the format row first and
  preselected — atop the server's actions, alone over an empty answer,
  alone with no server at all — and accepting it formats.
- The rows after it still run the server action they name.
