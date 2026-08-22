//! One running language server: its lifecycle, its pending requests, and the
//! capabilities it answered `initialize` with.
//!
//! The state machine is three phases. **Starting** — the process is up and
//! `initialize` is in flight; nothing else may be sent, which is why
//! `didOpen` waits (and, waiting, gets to carry whatever was typed in the
//! meantime for free). **Running** — capabilities are recorded and traffic
//! flows. **Dead** — the pipe closed or the handshake failed; nothing
//! restarts by itself, `:lsp restart` is the deliberate act.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use super::pos::{Encoding, uri_of};
use super::transport::{Spawn, Transport};
use super::types::{InitializeResult, SyncCaps};
use super::{Inbox, ServerId, rpc};

/// What to do with a response when it arrives. A closure would be shorter and
/// uninspectable; a variant names the continuation, and later features add
/// theirs beside these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    Initialize,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Starting,
    Running,
    Dead { reason: String },
}

/// One `$/progress` in flight — what `:lsp` shows while rust-analyzer indexes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub title: String,
    pub message: Option<String>,
    pub percentage: Option<u32>,
}

pub struct Client {
    pub id: ServerId,
    /// The config name — `rust-analyzer` — which is also how `:lsp` calls it.
    pub name: String,
    pub root: PathBuf,
    pub phase: Phase,
    pub encoding: Encoding,
    pub sync: SyncCaps,
    pub server_info: Option<String>,
    /// Keyed by the progress token, stringified — the token is the server's
    /// and may be a number or a string.
    pub progress: BTreeMap<String, Progress>,
    transport: Box<dyn Transport>,
    next_request: i64,
    pending: HashMap<i64, Intent>,
}

impl Client {
    /// Spawns the server and sends `initialize`. The client is `Starting`
    /// until the answer comes back through [`Client::finish_initialize`].
    pub fn start(
        id: ServerId,
        name: &str,
        command: &[String],
        root: &Path,
        inbox: Inbox,
        spawner: &dyn Spawn,
    ) -> Result<Self, String> {
        let transport = spawner.spawn(id, command, root, inbox)?;
        let mut client = Self {
            id,
            name: name.to_string(),
            root: root.to_path_buf(),
            phase: Phase::Starting,
            // The mandated default, until the server grants utf-8.
            encoding: Encoding::Utf16,
            sync: SyncCaps::default(),
            server_info: None,
            progress: BTreeMap::new(),
            transport,
            next_request: 1,
            pending: HashMap::new(),
        };

        let root_uri = uri_of(&client.root);
        let folder_name =
            client.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        client.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "clientInfo": { "name": "bi", "version": env!("CARGO_PKG_VERSION") },
                "rootUri": root_uri,
                "workspaceFolders": [{ "uri": root_uri, "name": folder_name }],
                "capabilities": {
                    "general": { "positionEncodings": ["utf-8", "utf-16"] },
                    "textDocument": {
                        "synchronization": { "didSave": true },
                        "publishDiagnostics": { "versionSupport": true },
                    },
                    "window": { "workDoneProgress": true },
                },
            }),
            Intent::Initialize,
        );
        Ok(client)
    }

    pub fn running(&self) -> bool {
        self.phase == Phase::Running
    }

    /// Sends a request and files what to do with its answer.
    pub fn request(&mut self, method: &str, params: Value, intent: Intent) {
        let id = self.next_request;
        self.next_request += 1;
        self.pending.insert(id, intent);
        self.transport.send(&rpc::request(id, method, params));
    }

    /// Sends a notification — silently dropped unless Running, which is the
    /// protocol's own rule about traffic before `initialized`. Core callers
    /// gate on phase anyway; this is the backstop.
    pub fn notify(&mut self, method: &str, params: Value) {
        if self.running() {
            self.transport.send(&rpc::notification(method, params));
        }
    }

    /// Answers a server's request. Not phase-gated: a server may legally ask
    /// during the handshake, and a request left dangling can deadlock it.
    pub fn respond(&mut self, msg: Value) {
        self.transport.send(&msg);
    }

    /// Claims the intent filed for a response id. `None` is an answer to a
    /// request nobody remembers, which is a message to drop.
    pub fn take_intent(&mut self, id: i64) -> Option<Intent> {
        self.pending.remove(&id)
    }

    /// The `initialize` answer arrived: record what was granted, tell the
    /// server bi is ready, and open the gate.
    pub fn finish_initialize(&mut self, result: Value) {
        let parsed: InitializeResult = serde_json::from_value(result).unwrap_or_default();
        if parsed.capabilities.position_encoding.as_deref() == Some("utf-8") {
            self.encoding = Encoding::Utf8;
        }
        self.sync = SyncCaps::parse(parsed.capabilities.text_document_sync.as_ref());
        self.server_info = parsed.server_info.map(|s| s.name);
        self.transport.send(&rpc::notification("initialized", json!({})));
        self.phase = Phase::Running;
    }

    /// The pipe closed, or the handshake failed. Keeps the transport — the
    /// stderr tail is the epitaph `:lsp` shows.
    pub fn die(&mut self, reason: String) {
        // Kill rather than leave: after a real exit this only reaps the
        // zombie, and after a protocol failure the process does not get to
        // outlive bi's opinion of it.
        self.transport.kill();
        self.phase = Phase::Dead { reason };
        self.pending.clear();
        self.progress.clear();
    }

    /// The exit code, if the process has exited.
    pub fn exit_status(&mut self) -> Option<i32> {
        self.transport.exit_status()
    }

    pub fn stderr_tail(&self) -> Vec<String> {
        self.transport.stderr_tail()
    }

    /// Asks the server to leave: `shutdown`, `exit`, a short wait, then the
    /// axe. The spec's shutdown-then-wait-for-the-response ceremony is for
    /// clients that will keep using the connection; bi is leaving.
    pub fn shutdown(&mut self, patience: Duration) {
        if let Phase::Dead { .. } = self.phase {
            return;
        }
        self.transport.send(&rpc::request(self.next_request, "shutdown", Value::Null));
        self.transport.send(&rpc::notification("exit", Value::Null));
        self.transport.wait_or_kill(patience);
        self.phase = Phase::Dead { reason: "shut down".into() };
    }
}
