//! A built-in debugger over the Debug Adapter Protocol. See `docs/specs/debug.md`.
//!
//! The layering mirrors `src/lsp/` deliberately: `types` and `rpc` are pure —
//! no process, no thread, no clock; `transport` is the only module that spawns
//! anything; `client` is one running session; `registry` is the set of them.
//! The editor stays the single owner of truth, exactly as with LSP.

pub mod rpc;
pub mod types;

/// One decoded message from the adapter, classified by DAP's `type` field
/// rather than LSP's id-vs-method scheme. Four shapes, because DAP's wire is
/// four things: an answer to a request bi sent, a spontaneous notification,
/// a request the *adapter* sends bi (rare, but real — `runInTerminal` is
/// the common one), and the pipe closing.
#[derive(Debug, Clone, PartialEq)]
pub enum Inbound {
    /// An answer to a request bi sent, matched back to it by `request_seq`.
    /// `success: false` still carries `command` and, often, `message` —
    /// the reason, not just the failure.
    Response {
        request_seq: i64,
        success: bool,
        command: String,
        body: serde_json::Value,
        message: Option<String>,
    },
    /// Something the adapter announces on its own — `stopped`, `output`,
    /// `terminated` — not an answer to anything bi asked.
    Event { event: String, body: serde_json::Value },
    /// A request in the other direction: the adapter asking bi to do
    /// something (e.g. `runInTerminal`) and waiting on a `response` built
    /// with `rpc::response`.
    ReverseRequest { seq: i64, command: String, arguments: serde_json::Value },
    /// The adapter closed its end of the pipe.
    Eof,
}
