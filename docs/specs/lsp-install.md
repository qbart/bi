# Installing a language server

`:lsp` can say a server is missing; this is the command that goes and gets
it. Minimal on purpose: there is no uniform "install a language server"
operation across ecosystems — Go and Rust have a toolchain one-liner, npm has
a package manager, clangd comes from the system and C3 from a git clone — and
pretending otherwise is how an editor grows a package manager of its own.

## Status

**Built.**

```
:lsp install           the server for this buffer's filetype
:lsp install go        the server claiming the named filetype
```

## The incantation is config, not code

The server table already says what to run and when; now it can say how to get
it:

```toml
[lsp.servers.gopls]
command = ["gopls"]
filetypes = ["go"]
roots = ["go.mod"]
install = ["go", "install", "golang.org/x/tools/gopls@latest"]
```

`install` is an argv, run as given — the ecosystem's own installer does the
work, bi just knows the incantation. Servers whose ecosystem has no
one-liner carry `install_hint` instead, a sentence for the status line:

```toml
[lsp.servers.clangd]
install_hint = "clangd ships with LLVM — install it with your package manager"
```

A server with neither gets a message pointing at the `install` field. Both
fields merge field-wise like the rest of the section, so overriding one keeps
the other.

**Neither is read from a project config.** The same refusal `command` makes,
for the same reason: a repository does not get to name a command bi runs —
and not a hint either, because a hint is an instruction you are being asked
to follow by whoever wrote the repo.

**What this deliberately does not do:** download binaries. The moment bi
fetches releases itself it inherits arch detection, updates, PATH precedence
and trust; the ecosystems already solved distribution, and where none did
(clangd, C3) the honest answer is a sentence, not a downloader. If a real
need outgrows this, that tier can be built later without moving anything
here.

## The command

`:lsp install` takes the focused buffer's filetype — you are always sitting
in the file whose server is missing, and typing the language's name again is
a keystroke charged for nothing. The optional argument covers the other case:
installing ahead of need, from anywhere.

The lookup is the attach predicate asked one more time: the enabled server
claiming the filetype. Then, in order: an install already running is
reported and nothing starts twice; an empty `install` prints the hint (or
the pointer at the field); otherwise the argv starts and the status line
shows it.

Installing when the server already runs is allowed and useful: the same
incantations update — `@latest` means it, `npm install -g` re-resolves.
`:lsp restart` picks the new binary up.

## Running, without blocking

An install can compile for minutes, and the fmt runner's guarded synchronous
call is the wrong shape for it. The core still spawns nothing:
`lsp::transport::Installer` is a frontend-supplied trait like `Spawn` and
`fmt::Run` —

```rust
pub trait Installer {
    fn begin(&self, argv: &[String], done: Box<dyn FnOnce(Result<(), String>) + Send>);
}
```

— begun and left. `ProcessInstall`, the shipped implementation, runs the
child on its own thread and calls `done` with `Ok` on exit 0 or the last
non-empty stderr line otherwise (installers narrate; the last line is the
verdict, where a formatter's first line was).

The callback fills a slot the editor polls in `settle` and rings the LSP
inbox's waker, so completion is a frame away, not a keystroke away — the
same editor-owns-all-truth arrangement every reader thread makes. One
install at a time; the slot is the whole queue.

**On success, attach asks again.** Every buffer resolved to "no server" goes
back to unresolved, and the same settle re-runs the attach pass — the binary
that was missing is the thing that just changed. On failure the status line
carries the stderr verdict, and nothing else moved.

A frontend that installs no `Installer` — a headless embedder — gets a
status line saying so, the same degradation as the spawner and the fmt
runner.

## Tests

- `install` and `install_hint` parse, merge field-wise, and are refused from
  a project config.
- `:lsp install` picks the server by the buffer's filetype; the argument
  overrides; an unclaimed filetype is a message.
- A hint-only server prints the hint; a bare one points at the field; no
  installer trait is a message.
- Completion: success reports, re-arms attach, and the pending slot clears;
  failure reports the stderr line. A second install while one runs is
  refused.
