# LSP core

Language servers, wired in the way the README promises: built in, not a
plugin. This spec is the *core* — server lifecycle, document sync, message
plumbing, and the `:lsp` command. Diagnostics are received and stored but not
yet drawn; hover, completion, go-to-definition and the rest are later features
that stand on this one and add no new architecture.

## Status

**Built.** Core only: lifecycle, sync, stored diagnostics, `:lsp`.

## The shape

Approach: a layered, sans-IO protocol core with I/O at the edges — the same
bet the rest of bi makes. The editor stays the single owner of truth; the only
concurrency is "bytes arrived on a queue".

```
src/lsp.rs            module root — ServerId, Inbox, the editor-facing surface
src/lsp/types.rs      the protocol structs bi uses, serde-derived      (pure)
src/lsp/rpc.rs        JSON-RPC envelope + Content-Length framing      (pure)
src/lsp/pos.rs        LSP Position ↔ rope offsets, path ↔ file:// URI (pure)
src/lsp/sync.rs       an Edit batch → one didChange content change    (pure)
src/lsp/transport.rs  the child process and its threads — the ONLY module
                      that spawns anything
src/lsp/client.rs     one running server: Starting → Running → Dead,
                      pending requests, negotiated capabilities
src/lsp/registry.rs   the set of clients, keyed (server name, root);
                      routing a buffer to a server
```

Dependencies added: `serde` (derive) and `serde_json`. The protocol types are
bi's own, not the `lsp-types` crate: core uses perhaps fifteen structs of a
protocol with hundreds, each later feature adds its own few, and owning the
shapes means they can be exactly what bi needs — no `Option` forests, no `url`
crate, no sixty-thousand-line dependency for the five percent used.

## Threads, the queue, and the waker

Two plain threads per server, owned by `transport.rs`:

- a **reader** that decodes frames off the server's stdout and delivers each
  message to the shared inbox;
- a **writer** that owns stdin and drains a channel of encoded frames, so the
  editor thread never blocks on a full pipe behind a busy server.

A third small thread drains stderr into a capped ring of lines, because the
first question about a server that died at startup is "what did it print".

The **inbox** is one `Mutex<VecDeque<(ServerId, Inbound)>>` shared by every
reader. After delivering, the reader calls the **waker** — a
`Fn() + Send + Sync` the frontend registered via `Editor::set_lsp_waker`, the
same handshake as `set_clipboard`: the library does not learn what an event
loop is. A headless embedder registers nothing and pumps on its own schedule.

The spawner crosses the boundary the same way. Spawning processes is a fact
about the host, so the frontend installs it — `main.rs` calls
`set_lsp_spawner(ProcessSpawn)` beside `set_clipboard` — and an embedder that
has no processes (a WASM sandbox, a test) supplies its own `Spawn` or nothing.
With none installed, attachment resolves to a reason `:lsp` reports, and no
process ever starts behind an embedder's back — which is also what keeps the
whole editor test suite from spawning real rust-analyzers.

There is **no event bus**. Messages become editor state inside `settle`;
future features file a request with a typed *intent* (an enum variant naming
what to do with the answer, never a closure), and the pump resolves the intent
when the response arrives. Inspectable, testable, no callback soup.

### The frontend loop

`main.rs` restructures once, for every async source there will ever be: a
spawned thread forwards `crossterm` events into an `mpsc` channel; the LSP
waker sends a wake token into the same channel; the main loop blocks on
`recv` (or `recv_timeout` while a flash is lit). True event-driven — zero
idle wakeups, zero added latency. Bursts drain through `try_recv` exactly as
they drained through `event::poll(ZERO)`.

## One server, one lifecycle

`ServerId` is a session-unique, never-reused counter, like `BufferId` and for
the same reason: a restarted server is a *new* instance, and a message queued
by the old one must not be mistaken for the new one's.

States: **Starting → Running → Dead**.

- **Starting**: the process is up and `initialize` is in flight. Outgoing
  notifications queue here and flush, in order, when the handshake completes —
  a `didOpen` sent before `initialize` answers is a protocol violation.
- **Running**: the `InitializeResult` arrived; capabilities are recorded
  (position encoding, sync kind, save); `initialized` was sent. Open and
  close are treated as universal — every real server wants them, and gating
  them on `openClose` would complicate the one queue that exists.
- **Dead**: stdout closed or the pipe errored. The exit status and the stderr
  tail are kept for `:lsp`. Nothing restarts automatically — a server that
  crashes on a particular file would crash-loop; `:lsp restart` is the
  deliberate act.

bi claims in `initialize`: `positionEncodings: ["utf-8", "utf-16"]` (prefer
utf-8 — rust-analyzer and clangd grant it, and then every position is the byte
column bi already has), `didSave`, `publishDiagnostics.versionSupport`, and
`window.workDoneProgress`.

Shutdown on quit sends `shutdown` and `exit` back to back, waits briefly, then
kills. The spec's shutdown-then-wait-for-response ceremony exists for clients
that will keep using the connection; bi is leaving, and every mainstream
server tolerates the short form. A server that ignores it is killed rather
than allowed to outlive its editor.

## Routing: which buffer gets which server

Config, a section like any other, a PATCH over built-in defaults:

```toml
[lsp]
enabled = true

[lsp.servers.rust-analyzer]
command = ["rust-analyzer"]
filetypes = ["rust"]
roots = ["Cargo.toml"]
```

Defaults ship for rust-analyzer, gopls, clangd, pyright,
typescript-language-server, lua-language-server and bash-language-server —
batteries included, and a binary that is not installed is a quiet fact `:lsp`
reports, not an error bi nags about. A user's `[lsp.servers.<name>]` merges
field-wise over the default of the same name, so overriding `command` alone
keeps the filetypes and roots; `enabled = false` inside one turns it off.

**Attachment is lazy and lives in `settle`**: any buffer with a path and a
filetype and no resolution yet gets one — find the server config claiming the
filetype, walk up from the file for a root marker (the server's own, then
`.git`, else the file's directory), then reuse or spawn the client keyed
`(server name, root)`. One key covers every buffer of a project; a second
project in the same session gets its own instance. The resolution — attached,
or "no server claims this filetype", or "spawn failed" — is cached on the
entry and revisited only when `config_epoch` moves, so the pass costs a
comparison per buffer per settle.

Scratch buffers have no path, hence no URI, hence no server.

## Document sync

`settle` was built for this: `pending_edits` was designed with two consumers,
and the second has arrived. The drain now feeds tree-sitter, then LSP, then
clears.

- **didOpen** at attachment, with the buffer's text and its filetype as
  `languageId`, version 1.
- **didChange** per settle batch, version +1 each. See below.
- **didSave** after a successful write. The write paths push the id onto
  `Session.pending_saves`, and settle drains it — the same
  record-then-drain shape as the edits themselves. `includeText` is honoured
  when the server registered for it.
- **didClose** when a buffer is deleted, or when `:w <path>` moves the file
  under the document. A restarted server's documents are not closed at the
  dying instance — it is being killed — they re-open on the fresh one through
  the ordinary attach path.

A server whose sync capability says Full gets the whole rope each time;
None gets nothing. Both are rare and both are theirs to ask for.

### Line-granular incremental changes

`didChange` ranges are **whole lines, always at character 0** — the trick
Neovim has shipped for years. Both encodings agree on column 0, so document
sync never converts a UTF-16 position, and it never needs *historical* text:
the replacement is read straight from the rope after the batch, so
`Buffer::Edit` grows no text field and stays `Copy`. `Buffer` is untouched by
this entire feature.

A batch composes into **one** replaced-line span by a fold over the edits.
Each `Edit`'s rows are in the document as of just before it; the running span
is `lo` (lines above it are untouched, so pre-batch and current line numbers
agree there), `old_hi` (exclusive, pre-batch), `new_hi` (exclusive, current).
For the next edit spanning pre-rows `[e_lo, e_old_hi)` and post-rows
`[e_lo, e_new_hi)`:

```
lo      = min(lo, e_lo)
old_hi += max(0, e_old_hi - new_hi)     // lines below the span map 1:1
new_hi  = max(new_hi, e_old_hi) + (e_new_hi - e_old_hi)
```

The change is then: range `(lo,0)..(old_hi,0)`, text = the rope's lines
`[lo, new_hi)`. When the span reaches the end of the file, `old_hi` may name
the line one past the last — the LSP spec defines that positions clamp, and
this exact shape is what every server already receives from Neovim.

The invariant test: apply the produced change to a shadow copy of the
pre-batch text and require the result to equal the rope, over hand-written
cases and a seeded sweep of random edit batches. A wrong composition is the
kind of bug that corrupts the server's view silently and surfaces as
diagnostics on the wrong lines a minute later; the shadow test is the one
assertion that makes it impossible.

## Inbound: the pump

`settle` ends by draining the inbox. Per message:

- **Responses** resolve the pending intent filed with the request id —
  `Initialize` and `Shutdown` today, `Definition { window }` and friends
  tomorrow. An unknown id is a response to a cancelled or forgotten request
  and is dropped.
- **Server→client requests** are answered, because a request left dangling
  can deadlock a server: `workspace/configuration` gets nulls,
  `client/registerCapability` and `window/workDoneProgress/create` get
  acknowledged, `workspace/applyEdit` reports not-applied, and anything else
  gets `MethodNotFound` — which is the honest answer and the one the protocol
  is designed around.
- **Notifications**: `publishDiagnostics` is stored (below);
  `window/showMessage` at Error or Warning lands on the status line;
  `$/progress` updates the client's progress table (`:lsp` shows it — the
  answer to "is rust-analyzer indexing or dead"); `window/logMessage` joins
  the stderr ring; the rest are dropped.
- **Eof** marks the client Dead and puts one line on the status — a crash is
  worth a heads-up; a missing binary at startup is not.

### Diagnostics, stored not drawn

Per attached buffer: the latest `publishDiagnostics`, converted on receipt to
**char ranges** through the negotiated encoding, kept beside the buffer where
`syntax` lives. A publish tagged with a version other than the current one is
stale — its successor is already being computed — and is dropped. Between
publishes, stored ranges are remapped through each settle's edits with
`Edit::map`, exactly as unfocused windows' selections are, so they never
drift from the text they annotate. The later diagnostics *feature* is
decorations and navigation over this state, nothing more.

## `:lsp`

- `:lsp` — one status line for the focused buffer:
  `rust-analyzer: running · ~/d/bi · utf-8 · 3 diagnostics · indexing 40%`,
  or `starting`, or `exited (code 101) — :lsp restart`, or why nothing is
  attached. The stderr tail backs the dead case.
- `:lsp restart` — kill this buffer's server instance, spawn a fresh one,
  re-open every document that was attached to it.
- `:lsp stop` — shut the instance down; its buffers detach and stay detached
  until `:lsp restart`.

## Testing

The layering is the test plan. `rpc`, `pos`, `sync` and `types` are pure:
framing round-trips and truncation, position clamping in both encodings, the
shadow-doc invariant, serde shapes pinned against captured server traffic.
`client` and `registry` run against a fake: spawning goes through a `Spawn`
trait (a real embedding needs this seam too — a WASM host has no processes),
so a test hands the registry a transport that records outgoing messages and
scripts inbound ones, and drives the whole handshake, queue-flush, capability
gating, death and restart without a process. Editor-level tests open files,
type, and assert on the didOpen/didChange stream and on stored diagnostics.
One smoke test exercises the real transport by spawning `/bin/cat`, which
echoes a frame back through the reader thread — process, pipes and threads
proven with no external dependency.

The `lib_boundary` test already guards the rest: `lsp` is library code, and
nothing in it may name a terminal.

## Deliberately not here

Auto-restart with backoff, `workspace/didChangeConfiguration`,
`workspace/didChangeWatchedFiles`, multiple servers per buffer, request
cancellation (`$/cancelRequest`), and every user-visible feature — inline
diagnostics, hover, completion, definition, formatting, rename. Each is a
later spec; none of them changes this one's shape: they file intents, read
capabilities, and draw decorations.
