//! Language servers, built in. See `docs/specs/lsp.md`.
//!
//! The layering: `types`, `rpc`, `pos` and `sync` are pure — no process, no
//! thread, no clock. `transport` is the only module that spawns anything.
//! `client` is one running server; `registry` is the set of them and the
//! routing. The editor stays the single owner of truth: reader threads do
//! nothing but put decoded messages on the [`Inbox`] and call the waker, and
//! everything becomes editor state inside `Editor::settle`.

pub mod client;
pub mod pos;
pub mod registry;
pub mod rpc;
pub mod sync;
pub mod transport;
pub mod types;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

pub use registry::{Effect, Registry, ServerConfig, SignatureData};

/// A running server instance's identity within a session.
///
/// Handed out monotonically and never reused, like `BufferId` and for the
/// same reason: a restarted server is a *new* instance, and a message queued
/// by the old one must not be mistaken for the new one's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ServerId(pub u32);

/// One decoded message from a server, classified but not yet interpreted.
#[derive(Debug)]
pub enum Inbound {
    /// An answer to a request bi sent. bi's ids are integers, so a response
    /// whose id is anything else matches no pending request and is dropped.
    Response {
        id: i64,
        result: Result<serde_json::Value, rpc::ResponseError>,
    },
    /// A request *from* the server. The id is echoed back verbatim — the
    /// server picks its shape, and a request left unanswered can deadlock it.
    Request {
        id: serde_json::Value,
        method: String,
        params: serde_json::Value,
    },
    Notification {
        method: String,
        params: serde_json::Value,
    },
    /// The server's stdout closed. The reader thread's last word.
    Eof,
}

/// Where reader threads put what arrived, and how the frontend hears of it.
///
/// One queue for every server, because the consumer is one editor thread. The
/// waker is whatever the frontend registered — for the terminal, a send on
/// the same channel its key events arrive on; for a headless embedder,
/// nothing, and it pumps on its own schedule.
#[derive(Clone, Default)]
pub struct Inbox {
    queue: Arc<Mutex<VecDeque<(ServerId, Inbound)>>>,
    #[allow(clippy::type_complexity, reason = "an alias would name it once and hide it")]
    waker: Arc<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>,
}

impl Inbox {
    /// Called from reader threads: queue the message, wake the frontend.
    pub fn deliver(&self, from: ServerId, msg: Inbound) {
        self.queue.lock().expect("inbox queue poisoned").push_back((from, msg));
        let waker = self.waker.lock().expect("inbox waker poisoned").clone();
        if let Some(wake) = waker {
            wake();
        }
    }

    /// Everything that has arrived, in order. Called from the editor thread.
    pub fn drain(&self) -> Vec<(ServerId, Inbound)> {
        self.queue.lock().expect("inbox queue poisoned").drain(..).collect()
    }

    pub fn set_waker(&self, wake: impl Fn() + Send + Sync + 'static) {
        *self.waker.lock().expect("inbox waker poisoned") = Some(Arc::new(wake));
    }
}

/// How bad a stored diagnostic is. LSP's four severities, as names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

impl Severity {
    /// From the wire's 1–4. Anything out of range reads as a hint: a server
    /// inventing severities is not a reason to drop what it said.
    pub fn from_wire(n: Option<u8>) -> Self {
        match n {
            Some(1) => Self::Error,
            Some(2) => Self::Warning,
            Some(3) => Self::Info,
            _ => Self::Hint,
        }
    }
}

/// The goto family: three requests of one shape. The kind picks the wire
/// method, the capability bit, and the word in every message; everything
/// downstream of the name is shared. See `docs/specs/lsp-requests.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Goto {
    /// `gd`, `:def`.
    Definition,
    /// `:decl` — the header's side of the question, where the languages
    /// split the two.
    Declaration,
    /// `:impl` — the bodies: trait impls, overrides, the source for a header.
    Implementation,
}

impl Goto {
    pub fn method(self) -> &'static str {
        match self {
            Self::Definition => "textDocument/definition",
            Self::Declaration => "textDocument/declaration",
            Self::Implementation => "textDocument/implementation",
        }
    }

    /// The word for messages: `no declaration found`.
    pub fn noun(self) -> &'static str {
        match self {
            Self::Definition => "definition",
            Self::Declaration => "declaration",
            Self::Implementation => "implementation",
        }
    }
}

/// One stored diagnostic, in **char offsets** into the buffer it annotates.
///
/// Converted from wire positions on receipt — through the encoding the server
/// was granted — and remapped through every subsequent edit exactly as an
/// unfocused window's selections are, so it never drifts from the text it
/// points at. The later diagnostics *feature* renders these; nothing draws
/// them yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diag {
    pub start: usize,
    pub end: usize,
    pub severity: Severity,
    pub message: String,
    pub source: Option<String>,
}

/// One buffer's attachment to a server: the document the server knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Doc {
    pub server: ServerId,
    /// The `file://` URI `didOpen` used. Kept rather than re-derived so the
    /// URI in `didClose` is byte-for-byte the one the server was given.
    pub uri: String,
    /// The version the server has been told, +1 per `didChange`. `i32`
    /// because that is the wire type.
    pub version: i32,
    /// Whether `didOpen` has gone out. False from attach until the handshake
    /// completes — see `Registry::try_open` for why the wait is a feature.
    pub opened: bool,
    pub diagnostics: Vec<Diag>,
}

/// Where a buffer stands with LSP. Lives on the buffer's entry, beside
/// `syntax`, and is resolved lazily in `Editor::settle`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Attach {
    /// Not looked at yet — the state a new buffer starts in.
    #[default]
    Unresolved,
    /// Looked at, and there is nothing to attach to. The reason is what
    /// `:lsp` reports. `epoch` is the config epoch the answer was computed
    /// under: when the config moves, the question is asked again.
    No {
        epoch: u64,
        reason: String,
    },
    Doc(Doc),
}
