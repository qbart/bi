//! One running debug session: its lifecycle, its pending requests, and the
//! capabilities the adapter answered `initialize` with.
//!
//! The state machine has five phases, and the handshake between them is
//! **event-driven** in a way LSP's is not — see `docs/specs/debug.md`
//! §"The lifecycle" for the wire-level ordering this mirrors. **Initializing**
//! — the process is up and `initialize` is in flight. Its *response* records
//! capabilities and immediately fires the stashed `launch`/`attach` (the
//! request name and body are opaque per the protocol — never modelled, only
//! passed through) — which moves to **Configuring**. Configuring waits for
//! the `initialized` *event* (not a response to anything bi sent) before the
//! editor may push breakpoints and call `configuration_done`; that is the
//! crucial fork against `lsp::Client`, where the initialize response alone
//! opens the gate. `configurationDone` starts or resumes the program, moving
//! to **Running**. From there a `stopped` event parks the session at
//! **Stopped { thread }** — the cue to walk `threads` → `stackTrace` →
//! `scopes` → `variables` — and `continued` returns it to Running. **
//! Terminated { reason }** is either end: a `terminated` event, a protocol
//! failure ([`Client::die`]), or a deliberate [`Client::disconnect`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use super::transport::{Spawn, Transport};
use super::types::Capabilities;
use super::{Inbox, SessionId, rpc};

/// What to do with a response when it arrives. Named rather than a closure,
/// same reasoning as `lsp::client::Intent`: a variant is inspectable and
/// later features add theirs beside these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    Initialize,
    Launch,
    Attach,
    ConfigurationDone,
    /// `setBreakpoints` for one file — the path names which file's gutter
    /// the answered `verified`/moved lines belong to.
    SetBreakpoints { path: PathBuf },
    Threads,
    /// The lazy chain's first link once a thread is known to be stopped.
    StackTrace { thread: i64 },
    /// The lazy chain's second link for one frame.
    Scopes { frame: i64 },
    /// The lazy chain's third link — also how a struct's fields or a `Vec`'s
    /// elements expand, against that value's own `variables_reference`.
    Variables { reference: i64 },
    /// A watch, the REPL, or a hover float — same request shape, different
    /// `context` and different UI on the answer.
    Evaluate { context: EvalContext, expr: String },
    Step,
    Continue,
    Pause,
    Disconnect,
}

/// `evaluate`'s `context` field — bi's three callers of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalContext {
    Watch,
    Repl,
    Hover,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Initializing,
    Configuring,
    Running,
    Stopped { thread: i64 },
    Terminated { reason: String },
}

pub struct Client {
    pub id: SessionId,
    /// The config name — `codelldb` — which is also how `:debug` calls it.
    pub name: String,
    pub root: PathBuf,
    pub phase: Phase,
    pub caps: Capabilities,
    transport: Box<dyn Transport>,
    next_seq: i64,
    pending: HashMap<i64, Intent>,
    /// Set by the `initialized` event; cleared never — once true the editor
    /// may call [`Client::configuration_done`] any number of times a session
    /// re-configures breakpoints while already running.
    ready_to_configure: bool,
    /// The launch/attach step, held from [`Client::start`] until the
    /// `initialize` response fires it in [`Client::finish_initialize`] — DAP
    /// forbids sending it any earlier. `(request, body)`: `request` is
    /// `"launch"` or `"attach"`; `body` is the adapter-specific JSON, passed
    /// through opaquely per the protocol's own design.
    stashed: Option<(String, Value)>,
}

impl Client {
    /// Spawns the adapter and sends `initialize`. The client is
    /// `Initializing` until the answer comes back through
    /// [`Client::finish_initialize`], which is also what sends the stashed
    /// `launch`/`attach`.
    #[allow(
        clippy::too_many_arguments,
        reason = "one session's whole identity plus the stashed launch step"
    )]
    pub fn start(
        id: SessionId,
        name: &str,
        command: &[String],
        root: &Path,
        inbox: Inbox,
        spawner: &dyn Spawn,
        request: &str,
        body: Value,
    ) -> Result<Self, String> {
        let transport = spawner.spawn(id, command, root, inbox)?;
        let mut client = Self {
            id,
            name: name.to_string(),
            root: root.to_path_buf(),
            phase: Phase::Initializing,
            caps: Capabilities::default(),
            transport,
            next_seq: 1,
            pending: HashMap::new(),
            ready_to_configure: false,
            stashed: Some((request.to_string(), body)),
        };
        client.request(
            "initialize",
            json!({
                "clientID": "bi",
                "adapterID": name,
                "linesStartAt1": true,
                "columnsStartAt1": true,
                "pathFormat": "path",
                "supportsRunInTerminalRequest": false,
            }),
            Intent::Initialize,
        );
        Ok(client)
    }

    pub fn running(&self) -> bool {
        matches!(self.phase, Phase::Running)
    }

    /// Whether the `initialized` event has arrived — the editor's gate to
    /// push breakpoints and call [`Client::configuration_done`].
    pub fn ready_to_configure(&self) -> bool {
        self.ready_to_configure
    }

    /// Sends a request and files what to do with its answer. Returns the
    /// sequence number, so a caller that needs to correlate a later event to
    /// this specific ask (rare — most correlation is by `take_intent`) can.
    pub fn request(&mut self, command: &str, args: Value, intent: Intent) -> i64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.pending.insert(seq, intent);
        self.transport.send(&rpc::request(seq, command, args));
        seq
    }

    /// Claims the intent filed for a response's `request_seq`. `None` is an
    /// answer to a request nobody remembers, which is a message to drop.
    pub fn take_intent(&mut self, seq: i64) -> Option<Intent> {
        self.pending.remove(&seq)
    }

    /// Answers a reverse request — one the adapter sent (e.g.
    /// `runInTerminal`) — allocating bi's own `seq` from the very counter
    /// `request` uses. DAP numbers every outbound message, requests and
    /// responses alike, from one sequence per side; a second counter here
    /// would hand out numbers `request` could also hand out, and the
    /// adapter would see the collision.
    pub fn respond(&mut self, request_seq: i64, command: &str, success: bool, body: Value) {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.transport.send(&rpc::response(request_seq, seq, command, success, body));
    }

    /// The `initialize` response arrived: record what the adapter granted,
    /// then fire the launch/attach that was waiting on it. Moves to
    /// `Configuring` — the program has not started yet; that waits for the
    /// `initialized` event and `configurationDone`, per the lifecycle.
    pub fn finish_initialize(&mut self, caps: Capabilities) {
        self.caps = caps;
        self.phase = Phase::Configuring;
        if let Some((request, body)) = self.stashed.take() {
            let intent = if request == "attach" { Intent::Attach } else { Intent::Launch };
            self.request(&request, body, intent);
        }
    }

    /// The `initialized` *event* — not a response to anything bi sent. This
    /// is the fork against LSP: the gate to push breakpoints and call
    /// `configuration_done` is an adapter-initiated announcement, not the
    /// answer to `initialize`.
    pub fn on_initialized_event(&mut self) {
        self.ready_to_configure = true;
    }

    /// Tells the adapter breakpoints are in and it may start or resume the
    /// program. A no-op before the `initialized` event: this is the crucial
    /// fork the whole module exists to defend — `configurationDone` must
    /// never go out ahead of it — so the gate is enforced here rather than
    /// left to callers' discipline, same backstop reasoning as
    /// `lsp::Client::notify`'s phase check.
    pub fn configuration_done(&mut self) {
        if !self.ready_to_configure {
            return;
        }
        self.request("configurationDone", Value::Null, Intent::ConfigurationDone);
        self.phase = Phase::Running;
    }

    /// A `stopped` event: the program is parked at `thread`, the cue to walk
    /// `threads` → `stackTrace` → `scopes` → `variables`.
    pub fn on_stopped(&mut self, thread: i64) {
        self.phase = Phase::Stopped { thread };
    }

    /// A `continued` event, or the local echo of a step/continue request
    /// that resumed the program without waiting for the adapter to confirm.
    pub fn on_continued(&mut self) {
        self.phase = Phase::Running;
    }

    /// The `terminated` event: the debuggee is gone. The adapter's process
    /// may still be alive — `disconnect` is the separate, deliberate act of
    /// ending the session with it.
    pub fn on_terminated(&mut self, reason: String) {
        self.phase = Phase::Terminated { reason };
    }

    /// The pipe closed, or the handshake failed. Keeps the transport — the
    /// stderr tail is the epitaph a debug UI shows.
    pub fn die(&mut self, reason: String) {
        // Kill rather than leave: after a real exit this only reaps the
        // zombie, and after a protocol failure the process does not get to
        // outlive bi's opinion of it.
        self.transport.kill();
        self.phase = Phase::Terminated { reason };
        self.pending.clear();
    }

    /// Asks the adapter to end the session: `disconnect`, optionally telling
    /// it to kill the debuggee too, then a short wait before the axe. Unlike
    /// `lsp::Client::shutdown`'s `shutdown`+`exit` pair, DAP has one request
    /// for this, with `terminateDebuggee` as its only real choice.
    pub fn disconnect(&mut self, terminate: bool, patience: Duration) {
        if let Phase::Terminated { .. } = self.phase {
            return;
        }
        self.request(
            "disconnect",
            json!({ "terminateDebuggee": terminate }),
            Intent::Disconnect,
        );
        self.transport.wait_or_kill(patience);
        self.phase = Phase::Terminated { reason: "disconnected".into() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dap::transport::fake::FakeSpawn;
    use crate::dap::{Inbox, SessionId};
    use serde_json::json;

    fn started(spawn: &FakeSpawn) -> Client {
        Client::start(
            SessionId(0), "codelldb", &["codelldb".into()],
            std::path::Path::new("/proj"), Inbox::default(), spawn,
            "launch", json!({ "program": "target/debug/bi" }),
        ).unwrap()
    }

    #[test]
    fn start_sends_initialize_first() {
        let spawn = FakeSpawn::default();
        let c = started(&spawn);
        assert!(matches!(c.phase, Phase::Initializing));
        assert_eq!(spawn.methods(SessionId(0)), vec!["initialize"]);
    }

    #[test]
    fn the_initialize_response_triggers_launch_with_the_opaque_body() {
        let spawn = FakeSpawn::default();
        let mut c = started(&spawn);
        c.finish_initialize(crate::dap::types::Capabilities::default());
        assert!(matches!(c.phase, Phase::Configuring));
        let launch = spawn.last(SessionId(0), "launch").expect("launch sent");
        // The body is passed through verbatim, never modelled.
        assert_eq!(launch["arguments"]["program"], "target/debug/bi");
    }

    #[test]
    fn configuration_done_waits_for_the_initialized_event() {
        let spawn = FakeSpawn::default();
        let mut c = started(&spawn);
        c.finish_initialize(Default::default());
        assert!(!c.ready_to_configure());
        c.on_initialized_event();
        assert!(c.ready_to_configure());
        c.configuration_done();
        assert!(spawn.methods(SessionId(0)).contains(&"configurationDone".to_string()));
    }

    #[test]
    fn configuration_done_before_the_initialized_event_is_a_no_op() {
        let spawn = FakeSpawn::default();
        let mut c = started(&spawn);
        c.finish_initialize(Default::default());
        c.configuration_done();
        assert!(!spawn.methods(SessionId(0)).contains(&"configurationDone".to_string()));
        assert!(matches!(c.phase, Phase::Configuring));

        c.on_initialized_event();
        c.configuration_done();
        assert!(spawn.methods(SessionId(0)).contains(&"configurationDone".to_string()));
        assert!(matches!(c.phase, Phase::Running));
    }

    #[test]
    fn a_stopped_event_records_the_thread() {
        let spawn = FakeSpawn::default();
        let mut c = started(&spawn);
        c.finish_initialize(Default::default());
        c.on_stopped(7);
        assert!(matches!(c.phase, Phase::Stopped { thread: 7 }));
    }

    #[test]
    fn continue_from_stopped_returns_to_running() {
        let spawn = FakeSpawn::default();
        let mut c = started(&spawn);
        c.finish_initialize(Default::default());
        c.on_stopped(7);
        c.on_continued();
        assert!(matches!(c.phase, Phase::Running));
    }
}
