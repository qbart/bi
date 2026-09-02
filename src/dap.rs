//! A built-in debugger over the Debug Adapter Protocol. See `docs/specs/debug.md`.
//!
//! The layering mirrors `src/lsp/` deliberately: `types` and `rpc` are pure —
//! no process, no thread, no clock; `transport` is the only module that spawns
//! anything; `client` is one running session; `registry` is the set of them.
//! The editor stays the single owner of truth, exactly as with LSP.

pub mod types;
