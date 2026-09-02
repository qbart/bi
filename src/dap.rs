//! A built-in debugger over the Debug Adapter Protocol. See `docs/specs/debug.md`.
//!
//! The layering mirrors `src/lsp/` deliberately: `types` and `rpc` are pure —
//! no process, no thread, no clock; `transport` is the only module that spawns
//! anything; `client` is one running session; `registry` is the set of them.
//! The editor stays the single owner of truth, exactly as with LSP.

pub mod client;
pub mod registry;
pub mod rpc;
pub mod transport;
pub mod types;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

pub use registry::{Breakpoint, Effect, Registry, Watch};

/// One `[debug.adapters.<name>]` — the argv that starts a DAP adapter.
///
/// Deliberately just the argv: unlike `lsp::ServerConfig` there is no
/// `filetypes`/`roots` (a debug session is started by naming a launch
/// configuration, never by opening a file) and no per-adapter `enabled`
/// (the one switch is `[debug]`'s own — an adapter nobody's launch config
/// names costs nothing to leave defined). See `docs/specs/debug.md`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AdapterConfig {
    pub command: Vec<String>,
}

/// One `[[debug.launch]]` — a named way to start or attach a debug session,
/// the project's own list rather than something merged over a built-in.
///
/// `body` is the DAP `launch`/`attach` request's `arguments`: every adapter
/// invents its own shape for it (`program`/`args`/`cwd` for codelldb,
/// `mode`/`program` for dlv), so bi never parses it — only the adapter does.
/// It travels from TOML to here to the wire opaquely, unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchConfig {
    pub name: String,
    /// Which `[debug.adapters.<name>]` starts it.
    pub adapter: String,
    /// `"launch"` or `"attach"` — DAP's only fork in how a session begins.
    pub request: String,
    pub body: serde_json::Value,
}

impl Default for LaunchConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            adapter: String::new(),
            request: String::new(),
            body: serde_json::Value::Null,
        }
    }
}

/// A running adapter instance's identity within a session.
///
/// Handed out monotonically and never reused, like `lsp::ServerId` and for
/// the same reason: a restarted adapter is a *new* instance, and a message
/// queued by the old one must not be mistaken for the new one's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(pub u32);

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

/// Where reader threads put what arrived, and how the frontend hears of it.
///
/// One queue for every session, because the consumer is one editor thread.
/// The waker is whatever the frontend registered — for the terminal, a send
/// on the same channel its key events arrive on; for a headless embedder,
/// nothing, and it pumps on its own schedule. A copy of `lsp::Inbox`, keyed
/// by `SessionId` instead of `ServerId` — the two protocols never share a
/// queue, since a debug session and a language server are different kinds
/// of thing even when the same editor thread drains both.
#[derive(Clone, Default)]
pub struct Inbox {
    queue: Arc<Mutex<VecDeque<(SessionId, Inbound)>>>,
    #[allow(clippy::type_complexity, reason = "an alias would name it once and hide it")]
    waker: Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>,
}

impl Inbox {
    /// Called from reader threads: queue the message, wake the frontend.
    pub fn deliver(&self, from: SessionId, msg: Inbound) {
        self.queue.lock().expect("inbox queue poisoned").push_back((from, msg));
        self.wake();
    }

    /// Rings the waker without a message — how a background completion that
    /// is not a session (e.g. a launch that resolved) gets the next settle
    /// to run rather than waiting for a keystroke.
    pub fn wake(&self) {
        let waker = self.waker.lock().expect("inbox waker poisoned").clone();
        if let Some(wake) = waker {
            wake();
        }
    }

    /// Everything that has arrived, in order. Called from the editor thread.
    pub fn drain(&self) -> Vec<(SessionId, Inbound)> {
        self.queue.lock().expect("inbox queue poisoned").drain(..).collect()
    }

    pub fn set_waker(&self, wake: impl Fn() + Send + Sync + 'static) {
        *self.waker.lock().expect("inbox waker poisoned") = Some(Arc::new(wake));
    }
}
