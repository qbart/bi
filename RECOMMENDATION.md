# Technology Recommendation: Batteries-Included Vim Editor

Goal: a Vim-style modal editor where tree-sitter, git, LSP, and fuzzy finding are
built in rather than bolted on as plugins.

## Recommendation: Rust

Everything in the goal has a first-class Rust story, and it's the only ecosystem
where all of it exists *together* without FFI glue becoming the main engineering
cost.

| Concern | Crate |
|---|---|
| Syntax / structural edits | `tree-sitter` (official bindings) + per-language grammar crates |
| Git | `git2` (libgit2 bindings) or `gitoxide` (pure Rust) |
| Text buffer | `ropey` (rope, UTF-8 + line indexing) |
| TUI | `ratatui` + `crossterm` |
| LSP client | `lsp-types` (+ your own async transport) |
| Fuzzy matching | `nucleo` (fzf-quality, written for Helix) |
| Async runtime | `tokio` |

**`git2` vs `gitoxide`:** start with `git2` — mature and complete. `gitoxide` is
faster and dependency-free but its API is still moving and coverage is uneven.
Both share the same conceptual model, so switching later stays contained if you
wrap git access behind your own trait.

## Know this before writing code

**Helix is already this project.** Modal editor in Rust, tree-sitter and LSP and
DAP built in, no plugin system required, `ropey` / `nucleo` / `tokio` under the
hood. Starting from scratch means spending the first year rebuilding it.

The one real gap: Helix is Kakoune-style (selection → action), not Vim-style
(action → motion). So the decision that actually matters:

- **Want Vim keys specifically** → fork Helix and replace the keymap/command
  layer. The hard parts (tree-sitter integration, LSP lifecycle, incremental
  reparse, rendering) are done and battle-tested; the input model is the layer
  you'd be rewriting anyway.
- **Want to build the whole thing yourself** → `ratatui` plus the table above,
  reading Helix's source as the reference implementation.

Other Rust prior art worth reading: **Zed** (GPU-rendered, tree-sitter, own git
layer — much bigger scope), **Lapce**.

## What actually bites you

Not tree-sitter or git — those are solved. The real design decisions:

1. **Config/extensibility language.** "All builtin" still needs config, and users
   will want *some* escape hatch. Helix went TOML-only for years and has been
   working on embedding Steel (a Scheme) because pure-declarative hit a ceiling.
   Decide early: TOML-only, embedded Lua (`mlua`), or WASM plugins. Retrofitting
   this is painful.
2. **Multi-language buffers.** Markdown with embedded code, JSX, Vue SFCs.
   Tree-sitter injections handle it, but the plumbing (per-region parsers,
   incremental reparse across injection boundaries) is where most of the
   complexity lives.
3. **LSP is stateful and hostile.** Servers crash, hang, and send diagnostics for
   stale versions. Version tracking and cancellation belong in the architecture
   from day one, not bolted on later.

## Ruling out the alternatives

- **Zig** — clean C interop with tree-sitter/libgit2 directly, but you'd write
  the rope, TUI, LSP client, and fuzzy matcher yourself, and the async story
  isn't settled.
- **Go** — tree-sitter needs cgo, which negates Go's build/deploy advantages, and
  its GC hurts editor latency tails. `go-git` is fine, though.
- **C++** — works (this is roughly the Neovim-adjacent path), but you pay for it
  in every other dimension.
