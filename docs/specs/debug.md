# Debug mode

A built-in debugger: set breakpoints in the gutter, launch or attach to a
program, step through it, and inspect variables, stack, and watches — without
leaving the editor. One integration, every language.

## Status

**Built.** `src/dap/` and the editor wiring (`Mode::Debug`, `DebugCmd`, the
gutter and panes, `[debug]` config) landed across fourteen tasks; six small
deviations from the text below are recorded at the end.

## Why DAP, and what each language actually needs

There is no per-language debugger integration worth building. Every road leads
to the **Debug Adapter Protocol**: bi implements one DAP client, and each
language becomes a config entry naming an adapter — the same bet LSP made,
with the same payoff.

The per-language landscape that forces this conclusion:

- **C++** — the native debuggers are gdb and lldb. Three historical routes in:
  scraping CLI output (fragile, dead end), **gdb/MI** — the old machine
  interface Emacs and early IDEs use (lldb-mi is semi-abandoned) — and
  **DAP**. Today both debuggers speak DAP directly: `lldb-dap` ships with
  LLVM, and gdb ≥ 14 has it built in (`gdb -i dap`). **CodeLLDB** is a
  standalone DAP adapter wrapping lldb with much better data visualization.
- **Rust** — there is no "Rust debugger". Rust compiles to native code with
  DWARF, so it is the C++ path: gdb or lldb. Everything `rust-gdb` /
  `rust-lldb` add is pretty-printer scripts for `Vec`, `String`, `Option`.
  The best adapter for Rust is **CodeLLDB** (built-in Rust formatters), with
  `lldb-dap` as the plain fallback.
- **Go** — gdb is explicitly discouraged (goroutines and the Go runtime
  confuse it). The debugger is **Delve**, and `dlv dap` speaks DAP natively.
  Goroutines surface as DAP "threads".
- **Free later** — Python (`debugpy`), JS (`js-debug`), C# (`netcoredbg`):
  all DAP, zero new architecture.

DAP differs from LSP only above the framing layer. Same Content-Length-framed
JSON over stdio; but every message carries `seq` and
`type: request | response | event`, responses carry `request_seq` + `success`,
and — unlike LSP — **events drive everything**. There are also *reverse
requests* (adapter → editor), chiefly `runInTerminal`.

## The lifecycle — how you know, and when to attach

The handshake is fixed by the protocol:

1. Spawn the adapter process (`codelldb`, `dlv dap`, `gdb -i dap`).
2. Send `initialize`; the response lists capabilities (conditional
   breakpoints? `configurationDone`? …). Gate features on these.
3. Send **either `launch` or `attach`** — the only fork:
   - **`launch`**: the *adapter* starts the program. The default, right ~90%
     of the time. The debuggee's stdout/stderr come back as `output` events.
   - **`attach`**: the process already exists — a running server to inspect
     (attach by **pid**; bi's picker becomes the process picker), or a
     remote/headless target (`gdbserver :1234`, `dlv attach --headless`, a
     container). Never guessed: the *launch configuration* declares
     `request = "launch"` or `request = "attach"` plus `program` / `pid` /
     `host`. Protocol quirk that shapes config: the launch/attach body is
     deliberately **not standardized** — it is adapter-specific JSON — so
     bi passes it through opaquely and never models it.
4. The adapter sends the **`initialized` event**. *This* is the moment: now
   push all breakpoints (`setBreakpoints` per file,
   `setExceptionBreakpoints`), then `configurationDone`. The program
   starts or resumes.
5. From here it is event-driven. A `stopped` event (reason: breakpoint /
   step / pause / exception, plus `threadId`) is the cue to fetch state,
   always lazily down one chain:
   `threads` → `stackTrace(thread)` → `scopes(frame)` → `variables(ref)`.
   Variables form a lazy tree — expanding a struct means requesting its
   `variablesReference`. **Watches** are just
   `evaluate { expr, frameId, context: "watch" }`, re-run on every stop.
6. Stepping: `next` (step over), `stepIn`, `stepOut`, `continue`, `pause`.
7. End: `terminated` event arrives; the client sends `disconnect`
   (optionally `terminateDebuggee`).

**Breakpoints are editor-owned.** They can be set any time, with no session
running; they are pushed at `initialized` and re-pushed live via
`setBreakpoints` while running. The adapter answers with *verified* flags and
possibly **moved lines** (no code on the requested line) — the gutter sign
must reflect both.

## The shape in bi

`src/dap/` mirrors `src/lsp/` layer for layer — sans-IO core, I/O at the
edges, editor as single owner of truth:

```
src/dap.rs            module root — SessionId, Inbox, editor-facing surface
src/dap/types.rs      the protocol structs bi uses, serde-derived      (pure)
src/dap/rpc.rs        seq/request/response/event envelope; framing
                      shared with lsp/rpc.rs (identical Content-Length) (pure)
src/dap/transport.rs  the adapter child process — reuses the Transport /
                      Spawn traits and thread trio from lsp/transport.rs
src/dap/client.rs     one session: Initializing → Configuring → Running
                      → Stopped(thread) → Terminated; pending requests
                      as typed intents, capabilities
src/dap/registry.rs   sessions + editor-owned breakpoint store; Effect
                      enum back into settle()
```

The main loop gains `Wake::Dap` beside `Wake::Term` and `Wake::Lsp` — the
loop was built for exactly this ("every async source there will ever be joins
by sending into it"). Messages become editor state inside `settle()`; no
event bus, no closures, typed intents only. The spawner and waker cross the
lib boundary via `set_dap_spawner` / `set_dap_waker`, same handshake as LSP,
so embedders and tests never spawn real adapters.

## Mode

`Mode::Debug` is a real `Mode` variant, entered explicitly (e.g. `:debug` /
a key), active only while it is useful. In it, plain keys drive the session
over the source window:

- `c` continue, `n` step over, `s` step in, `o` step out, `p` pause
- `b` toggle breakpoint on the cursor line
- `K` evaluate the expression under the cursor — the answer floats over the
  cursor in the same float LSP's own `K` uses (`Effect::Evaluated { context:
  Hover, .. }`, no status-line fallback)
- `Esc` back to Normal for editing; `:break` toggles the same breakpoint from
  Normal (or anywhere), so setting one never requires the mode — `b` itself
  is not rebound in Normal, where it already means `word_backward`

`:debug attach [name]` resolves `name` (or the single `request = "attach"`
launch config there is) and opens bi's picker as the process picker — every
readable `/proc` entry, one pid per row; accepting attaches. `:debug
attach-pid <n>` is the platform-free path underneath it: same config
resolution, no picker, attaches straight to `n`.

It needs the usual four touches: a `Mode` variant + `label()`, arms in
`input.rs`, a `KeyMode` for `[keys.debug]`, and dispatch through a new
`DebugCmd` sub-command enum beside `LspCmd` / `TreeCmd`.

## UI

- **Gutter**: breakpoint sign `●` (dim variant when unverified/moved,
  distinct when conditional), a new tenant in `gutter_signs` with priority
  above diagnostics. Stopped line: `▶` sign plus a `Repaint` decoration for
  the whole line. New theme keys for all of it.
- **Panes**: new `ContentKind` variants rendered like Tree/Results —
  **Variables** (lazy expandable tree; scopes at the root),
  **Stack** (frames; enter jumps source to the frame),
  **Console** (`output` events + an `evaluate` REPL line),
  **Watches** (expressions re-evaluated on every stop).
- Frame navigation moves the source window and re-scopes Variables/Watches
  to the selected frame.
- `:debug stack` / `:debug console` / `:debug vars` / `:debug watches` open
  the named pane in a split, or focus it if one is already open. All four
  use the tree pane's keymap: `j`/`k`/`gg`/`G`/`Ctrl-d`/`Ctrl-u` move the
  selection, `Enter` (or `i` on Console) acts. Variables adds `l`/`h` to
  expand/collapse a node — expanding an unloaded one fetches its children
  lazily. Watches adds `a` to prefill `:watch ` and `d` to remove the
  selected watch.
- `:eval <expr>` is the Console pane's REPL line: evaluates in the current
  frame while stopped and appends `"> {expr}"` and the result to every open
  console. `:watch <expr>` adds an expression to the Watches list
  (re-evaluated on every stop, and immediately if already stopped);
  `:unwatch <n>` removes the `n`th watch, 1-based as the pane numbers rows.

## Config

`[debug]` in config plus project-local launch configurations (the
local-config layer), each naming an adapter command and an opaque
launch/attach body:

```toml
[[debug.launch]]
name = "run tests"
adapter = "codelldb"          # [debug.adapters.codelldb] gives the command
request = "launch"            # or "attach"
body = { program = "target/debug/bi", args = [] }   # passed through verbatim
```

Default-blessed adapters: CodeLLDB (Rust/C++), `dlv dap` (Go), `gdb -i dap`
as the no-install fallback on new-enough systems.

## v1 scope and punts

- **In**: launch; attach by pid (cheap given the picker); breakpoints with
  verified/moved handling; step/continue/pause; stack, scopes, variables,
  watches, console output; evaluate-under-cursor.
- **Punted**: `runInTerminal` and any pty (bi has none) — interactive stdin
  for the debuggee waits; `output` events cover non-interactive Rust/Go/C++
  fine. Also punted: remote attach, conditional/function/exception
  breakpoint UI beyond pass-through, multi-session (`startDebugging`),
  disassembly views.

## Resolved decisions

1. **Mode shape** — full `Mode::Debug` as above.
2. **v1 attach** — launch + attach-by-pid.
3. **Panes** — dedicated persistent panes.
4. **Default adapters** — CodeLLDB + `dlv dap` + `gdb -i dap` (gdb ≥ 14).

## Deviations from the design

Six places where the build settled on something narrower or different than
this spec's text implies, each for a concrete reason:

1. **`b` is not bound in Normal mode.** Normal already owns it for
   `word_backward`, and stealing it would break a motion nobody asked to
   lose. `:break` is the Normal-reachable toggle instead — `b` still works
   as the breakpoint toggle, but only inside `Mode::Debug`.
2. **`stopped` goes straight to `stackTrace`, skipping `threads`.** v1 has
   no Threads pane to populate, and every adapter bi ships accepts a
   `threadId` straight off the `stopped` event, so the round-trip would
   have bought nothing yet.
3. **The Console's REPL line is `:eval`, not an in-pane input.** bi has no
   text-entry widget inside a pane — every other REPL-like surface (`:s`,
   `:find`) is already the ex line — so reusing it kept the Console a pure
   display and avoided inventing a second input mechanism for one pane.
4. **`[debug.adapters.*].command` is refused from project-local config.**
   Same trap as `[lsp.servers.*].command` and `[fmt.tools.*].command`: a
   repository that can name the binary bi spawns is arbitrary code
   execution by `git clone`. `[debug]`'s own `enabled` and
   `[[debug.launch]]` are still read, since launch configs are inherently
   project-local — the whole point of the section.
5. **`[[debug.launch]]` dedups by `name` across config layers.** A project
   overriding "run tests" should replace it, not stack a second entry with
   the same label in the picker — later layer (local over main over
   defaults) wins the slot.
6. **Theme keys are flat snake_case** (`debug_breakpoint`,
   `debug_breakpoint_unverified`, `debug_breakpoint_conditional`,
   `debug_stopped`), not dotted. Every existing theme key in bi is flat;
   a dotted `debug.breakpoint` would have been the only one and bought
   nothing but inconsistency.
