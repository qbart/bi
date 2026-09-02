//! The set of running debug sessions, and the editor-owned breakpoint store.
//!
//! Mirrors `lsp::registry` in shape — the registry owns the [`Inbox`]
//! transports deliver into, and [`Registry::pump`] turns everything that has
//! arrived into client bookkeeping plus, when the editor must act, a batch of
//! [`Effect`]s. The registry never touches editor state itself.
//!
//! **Breakpoints are editor-owned and session-independent** (per
//! `docs/specs/debug.md` §"The lifecycle"): the store lives here, keyed by
//! path, and survives a session ending or never having started. `line` is
//! always the 0-based buffer row bi's own cursor/gutter use; the wire's
//! 1-based line is converted at the edge, in [`Registry::set_verified`].
//!
//! **The `initialized` → `configurationDone` handoff.** The lifecycle
//! requires every breakpoint to be pushed (`setBreakpoints`, one request per
//! file) before `configurationDone` — and forbids sending it any earlier.
//! Rather than lean on the editor to count acknowledgements across settles,
//! the registry counts them itself: on `initialized`, it notes how many
//! files it is about to ask the editor to push (via
//! [`Effect::PushBreakpoints`]) in `pending_pushes`, and each matching
//! `setBreakpoints` response — routed back here through
//! `Intent::SetBreakpoints`, regardless of the file's own success or
//! failure — ticks it down. Reaching zero calls `configuration_done` on the
//! session directly. **When there is nothing to push**, `pending_pushes`
//! would never move, so `pump` special-cases it: zero files means
//! `configuration_done` fires immediately, in the same `initialized`
//! handling, no effect needed. This makes the signal entirely internal —
//! the editor's only job is to answer each `PushBreakpoints` effect with a
//! `setBreakpoints` request carrying `Intent::SetBreakpoints { path }`, and
//! the registry takes it from there.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::client::{Client, EvalContext, Intent};
use super::transport::Spawn;
use super::{Inbound, Inbox, SessionId, types};

/// One breakpoint as bi's gutter shows it. `line` is the 0-based row it was
/// set at; `moved_to`, when present, is where the adapter actually placed it
/// (the requested line had no code) — the gutter sign belongs at
/// `moved_to.unwrap_or(line)`. `conditional` is carried for the UI beyond
/// pass-through (v1 punts the condition-editing flow itself, per
/// `docs/specs/debug.md`) and defaults to `false`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Breakpoint {
    pub line: usize,
    pub verified: bool,
    pub moved_to: Option<usize>,
    pub conditional: bool,
}

/// What the editor must do about an inbound message — everything else
/// (client phase transitions, filed intents, breakpoint bookkeeping) is
/// handled inside [`Registry::pump`] before it ever reaches here.
#[derive(Debug)]
pub enum Effect {
    /// A line for the status bar — a crash, or an unsupported reverse
    /// request answered so the adapter is not left hanging.
    Status(String),
    /// A `stopped` event: the cue to walk
    /// `threads` → `stackTrace` → `scopes` → `variables`.
    Stopped { session: SessionId, thread: i64 },
    /// The `initialized` event, for one file that has breakpoints: send
    /// `setBreakpoints` for it, tagged `Intent::SetBreakpoints { path }` so
    /// the answer routes back to [`Registry::pump`]. See the module doc for
    /// how the registry itself tracks when every file has answered.
    PushBreakpoints { path: PathBuf },
    Threads(Vec<types::Thread>),
    Stack { frames: Vec<types::StackFrame> },
    Scopes(Vec<types::Scope>),
    Variables { reference: i64, vars: Vec<types::Variable> },
    Output { category: String, text: String },
    Evaluated { context: EvalContext, expr: String, result: String },
    /// Either end of a session: a `terminated` event, or the pipe closing
    /// ([`Inbound::Eof`]) which `pump` also treats as a death.
    Terminated { session: SessionId, reason: String },
}

pub struct Registry {
    inbox: Inbox,
    /// How adapters come to exist — supplied by the frontend, exactly as
    /// `lsp::Registry::spawner` is: a test or a process-less embedding
    /// supplies its own or nothing.
    spawner: Option<Box<dyn Spawn>>,
    sessions: Vec<Client>,
    next_id: u32,
    /// Editor-owned, session-independent: set any time, survives a session
    /// ending, keyed by path so a file's breakpoints outlive any one debug
    /// run. See the module doc.
    breakpoints: BTreeMap<PathBuf, Vec<Breakpoint>>,
    /// The session plain keys (`c`/`n`/`s`/`o`/`p`/`b` in `Mode::Debug`, per
    /// the spec) act on — the most recently launched, until v1's single
    /// foreground session grows a switcher.
    active: Option<SessionId>,
    /// Sessions with `setBreakpoints` answers still outstanding from the
    /// `initialized` push, counted down to the `configurationDone` it gates.
    /// See the module doc for why this lives here rather than in the editor.
    pending_pushes: HashMap<SessionId, usize>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            inbox: Inbox::default(),
            spawner: None,
            sessions: Vec::new(),
            next_id: 0,
            breakpoints: BTreeMap::new(),
            active: None,
            pending_pushes: HashMap::new(),
        }
    }
}

impl Registry {
    /// The inbox transports deliver into — where the frontend's waker is
    /// registered, and where a test answers as the adapter.
    pub fn inbox(&self) -> &Inbox {
        &self.inbox
    }

    /// Replaces how adapters come to exist — a test's fake, or a
    /// process-less embedding's own.
    pub fn set_spawner(&mut self, spawner: impl Spawn + 'static) {
        self.spawner = Some(Box::new(spawner));
    }

    /// The session `Mode::Debug`'s keys act on.
    pub fn active(&self) -> Option<&Client> {
        let id = self.active?;
        self.sessions.iter().find(|c| c.id == id)
    }

    pub fn active_mut(&mut self) -> Option<&mut Client> {
        let id = self.active?;
        self.sessions.iter_mut().find(|c| c.id == id)
    }

    /// One running or finished session by id — how the editor resolves the
    /// `session` an [`Effect::Stopped`] or [`Effect::Terminated`] names when
    /// it is not (or no longer) the active one.
    pub fn session(&self, id: SessionId) -> Option<&Client> {
        self.sessions.iter().find(|c| c.id == id)
    }

    /// Toggles one breakpoint at `path`:`line` (0-based row). Returns
    /// whether it is now set. Pure bookkeeping — pushing it to a live
    /// session's adapter is the editor's job, driven by
    /// [`Effect::PushBreakpoints`] or a direct `setBreakpoints` call while
    /// running.
    pub fn toggle_breakpoint(&mut self, path: &Path, line: usize) -> bool {
        let list = self.breakpoints.entry(path.to_path_buf()).or_default();
        match list.iter().position(|b| b.line == line) {
            Some(at) => {
                list.remove(at);
                if list.is_empty() {
                    self.breakpoints.remove(path);
                }
                false
            }
            None => {
                list.push(Breakpoint { line, verified: false, moved_to: None, conditional: false });
                true
            }
        }
    }

    /// Every breakpoint set in `path`, in the order a `setBreakpoints`
    /// request sends them — the order [`Registry::set_verified`]'s `results`
    /// must answer back in.
    pub fn breakpoints_for(&self, path: &Path) -> &[Breakpoint] {
        self.breakpoints.get(path).map_or(&[], Vec::as_slice)
    }

    /// Marks the adapter's `setBreakpoints` answer onto the stored rows, in
    /// the order they were asked. The wire's 1-based `line` becomes a
    /// 0-based `moved_to`, kept only when it actually differs from the row
    /// that was requested — an adapter that dutifully echoes the unmoved
    /// line back should not read as "moved".
    pub fn set_verified(&mut self, path: &Path, results: &[types::Breakpoint]) {
        let Some(list) = self.breakpoints.get_mut(path) else { return };
        for (bp, result) in list.iter_mut().zip(results) {
            bp.verified = result.verified;
            bp.moved_to = match result.line {
                Some(wire) => {
                    let row = wire.max(1) as usize - 1;
                    (row != bp.line).then_some(row)
                }
                None => None,
            };
        }
    }

    /// Spawns the adapter and sends `initialize`, per the lifecycle's first
    /// step. `request`/`body` are the stashed `launch`/`attach` — sent once
    /// `initialize` answers, never modelled (protocol-opaque, per
    /// `docs/specs/debug.md`). The new session becomes
    /// [`Registry::active`].
    pub fn launch(
        &mut self,
        name: &str,
        command: &[String],
        root: &Path,
        request: &str,
        body: Value,
    ) -> Result<SessionId, String> {
        let Some(spawner) = self.spawner.as_deref() else {
            return Err("this frontend supplies no debug adapter spawner".into());
        };
        let id = SessionId(self.next_id);
        self.next_id += 1;
        let client =
            Client::start(id, name, command, root, self.inbox.clone(), spawner, request, body)?;
        self.sessions.push(client);
        self.active = Some(id);
        Ok(id)
    }

    /// Drains the inbox and advances every session whose turn has come,
    /// returning what only the editor can do. See the module doc for how
    /// `initialized` → `configurationDone` is handled without the editor's
    /// help.
    pub fn pump(&mut self) -> Vec<Effect> {
        let mut effects = Vec::new();
        for (from, msg) in self.inbox.drain() {
            effects.extend(self.accept(from, msg));
        }
        effects
    }

    fn accept(&mut self, from: SessionId, msg: Inbound) -> Vec<Effect> {
        match msg {
            Inbound::Response { request_seq, success, command, body, message } => {
                self.accept_response(from, request_seq, success, &command, body, message)
            }
            Inbound::Event { event, body } => self.accept_event(from, &event, body),
            Inbound::ReverseRequest { seq, command, .. } => {
                self.accept_reverse_request(from, seq, &command)
            }
            Inbound::Eof => {
                let Some(client) = self.sessions.iter_mut().find(|c| c.id == from) else { return Vec::new() };
                let reason = "pipe closed".to_string();
                client.die(reason.clone());
                vec![Effect::Terminated { session: from, reason }]
            }
        }
    }

    fn accept_response(
        &mut self,
        from: SessionId,
        request_seq: i64,
        success: bool,
        command: &str,
        body: Value,
        message: Option<String>,
    ) -> Vec<Effect> {
        let Some(client) = self.sessions.iter_mut().find(|c| c.id == from) else { return Vec::new() };
        let Some(intent) = client.take_intent(request_seq) else { return Vec::new() };

        // The common failure shape for the requests that only fire an
        // effect on success — a crash reason, not a scold, since most of
        // these are read-only lookups a UI silently retries.
        let failed = |what: &str| {
            vec![Effect::Status(format!(
                "{what}: {}",
                message.clone().unwrap_or_else(|| "failed".into())
            ))]
        };

        match intent {
            Intent::Initialize => {
                if success {
                    let caps: types::Capabilities = serde_json::from_value(body).unwrap_or_default();
                    client.finish_initialize(caps);
                    Vec::new()
                } else {
                    let name = client.name.clone();
                    let reason = message.unwrap_or_else(|| "initialize failed".into());
                    client.die(reason.clone());
                    vec![Effect::Status(format!("{name}: {reason} — :debug"))]
                }
            }
            // `launch`/`attach` only matter on failure — success is silent,
            // since the actual "the program is up" cue is the `initialized`
            // event, not this response (the fork the whole client exists to
            // defend; see `client::Client`'s doc).
            Intent::Launch | Intent::Attach => {
                if success {
                    Vec::new()
                } else {
                    let name = client.name.clone();
                    let reason = message.unwrap_or_else(|| format!("{command} failed"));
                    client.die(reason.clone());
                    vec![Effect::Status(format!("{name}: {reason}"))]
                }
            }
            // Nothing to do: the phase already moved to Running when this
            // was sent, and a failure here means the adapter rejected a
            // well-formed no-argument request — not actionable.
            Intent::ConfigurationDone => Vec::new(),
            Intent::SetBreakpoints { path } => {
                if success {
                    let arr = body.get("breakpoints").cloned().unwrap_or(Value::Array(Vec::new()));
                    let results: Vec<types::Breakpoint> =
                        serde_json::from_value(arr).unwrap_or_default();
                    if let Some(list) = self.breakpoints.get_mut(&path) {
                        apply_verified(list, &results);
                    }
                }
                // Counted whether or not it succeeded — a rejected file
                // still answered, and `configurationDone` must not wait
                // forever on one adapter's complaint. See the module doc.
                if let Some(pending) = self.pending_pushes.get_mut(&from) {
                    *pending = pending.saturating_sub(1);
                    if *pending == 0 {
                        self.pending_pushes.remove(&from);
                        if let Some(client) = self.sessions.iter_mut().find(|c| c.id == from) {
                            client.configuration_done();
                        }
                    }
                }
                Vec::new()
            }
            Intent::Threads => {
                if success {
                    let arr = body.get("threads").cloned().unwrap_or(Value::Array(Vec::new()));
                    vec![Effect::Threads(serde_json::from_value(arr).unwrap_or_default())]
                } else {
                    failed("threads")
                }
            }
            Intent::StackTrace { .. } => {
                if success {
                    let arr = body.get("stackFrames").cloned().unwrap_or(Value::Array(Vec::new()));
                    vec![Effect::Stack { frames: serde_json::from_value(arr).unwrap_or_default() }]
                } else {
                    failed("stack")
                }
            }
            Intent::Scopes { .. } => {
                if success {
                    let arr = body.get("scopes").cloned().unwrap_or(Value::Array(Vec::new()));
                    vec![Effect::Scopes(serde_json::from_value(arr).unwrap_or_default())]
                } else {
                    failed("scopes")
                }
            }
            Intent::Variables { reference } => {
                if success {
                    let arr = body.get("variables").cloned().unwrap_or(Value::Array(Vec::new()));
                    let vars = serde_json::from_value(arr).unwrap_or_default();
                    vec![Effect::Variables { reference, vars }]
                } else {
                    failed("variables")
                }
            }
            Intent::Evaluate { context, expr } => {
                if success {
                    let result = body.get("result").and_then(Value::as_str).unwrap_or("").to_string();
                    vec![Effect::Evaluated { context, expr, result }]
                } else {
                    failed("evaluate")
                }
            }
            // The local echo the protocol allows: a successful response
            // means the debuggee resumed even if no separate `continued`
            // event follows. See `Client::on_continued`'s doc.
            Intent::Step | Intent::Continue => {
                if success {
                    client.on_continued();
                }
                Vec::new()
            }
            // `pause` and `disconnect` need no response handling: a pause
            // resolves via the `stopped` event that follows, and
            // `disconnect` already moved the phase to Terminated before the
            // request went out (see `Client::disconnect`).
            Intent::Pause | Intent::Disconnect => Vec::new(),
        }
    }

    fn accept_event(&mut self, from: SessionId, event: &str, body: Value) -> Vec<Effect> {
        match event {
            "initialized" => {
                let Some(client) = self.sessions.iter_mut().find(|c| c.id == from) else { return Vec::new() };
                client.on_initialized_event();
                let files: Vec<PathBuf> = self.breakpoints.keys().cloned().collect();
                if files.is_empty() {
                    // Nothing to push, so nothing will ever tick
                    // `pending_pushes` down — the gate opens right here.
                    client.configuration_done();
                    Vec::new()
                } else {
                    self.pending_pushes.insert(from, files.len());
                    files.into_iter().map(|path| Effect::PushBreakpoints { path }).collect()
                }
            }
            "stopped" => {
                let Some(client) = self.sessions.iter_mut().find(|c| c.id == from) else { return Vec::new() };
                match serde_json::from_value::<types::StoppedEvent>(body) {
                    Ok(ev) => {
                        let thread = ev.thread_id.unwrap_or(0);
                        client.on_stopped(thread);
                        vec![Effect::Stopped { session: from, thread }]
                    }
                    Err(_) => Vec::new(),
                }
            }
            "continued" => {
                let Some(client) = self.sessions.iter_mut().find(|c| c.id == from) else { return Vec::new() };
                client.on_continued();
                Vec::new()
            }
            "output" => match serde_json::from_value::<types::OutputEvent>(body) {
                Ok(ev) => {
                    let category = ev.category.unwrap_or_else(|| "console".into());
                    vec![Effect::Output { category, text: ev.output }]
                }
                Err(_) => Vec::new(),
            },
            "terminated" => {
                let Some(client) = self.sessions.iter_mut().find(|c| c.id == from) else { return Vec::new() };
                let reason = "terminated".to_string();
                client.on_terminated(reason.clone());
                vec![Effect::Terminated { session: from, reason }]
            }
            // Adapters send others bi does not model yet (`thread`,
            // `module`, `breakpoint` …) — silently ignored, same stance as
            // `lsp::Registry::accept`'s unmatched notifications.
            _ => Vec::new(),
        }
    }

    /// A reverse request — the adapter asking bi to do something. v1 speaks
    /// none of them (`runInTerminal` is the only common one, and bi has no
    /// pty to back it — see `docs/specs/debug.md`'s punts), so every one is
    /// answered `success: false` rather than left to time out, with a
    /// status line only for the named case a user might otherwise wait on.
    fn accept_reverse_request(&mut self, from: SessionId, seq: i64, command: &str) -> Vec<Effect> {
        let Some(client) = self.sessions.iter_mut().find(|c| c.id == from) else { return Vec::new() };
        client.respond(seq, command, false, Value::Null);
        if command == "runInTerminal" {
            vec![Effect::Status(format!(
                "{}: runInTerminal is unsupported — bi has no pty; the debuggee's output \
                 still arrives as output events",
                client.name
            ))]
        } else {
            Vec::new()
        }
    }
}

/// [`Registry::set_verified`]'s inner loop, factored out so it can run
/// against `self.breakpoints`' entry directly without a `&mut self` method
/// call fighting the `&mut Client` borrow live in [`Registry::accept_response`].
fn apply_verified(list: &mut [Breakpoint], results: &[types::Breakpoint]) {
    for (bp, result) in list.iter_mut().zip(results) {
        bp.verified = result.verified;
        bp.moved_to = match result.line {
            Some(wire) => {
                let row = wire.max(1) as usize - 1;
                (row != bp.line).then_some(row)
            }
            None => None,
        };
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;

    use super::*;
    use crate::dap::transport::fake::FakeSpawn;

    #[test]
    fn breakpoints_toggle_on_and_off_without_a_session() {
        let mut reg = Registry::default();
        assert!(reg.toggle_breakpoint(Path::new("/a.rs"), 10));
        assert_eq!(reg.breakpoints_for(Path::new("/a.rs")).len(), 1);
        assert!(!reg.toggle_breakpoint(Path::new("/a.rs"), 10));
        assert!(reg.breakpoints_for(Path::new("/a.rs")).is_empty());
    }

    #[test]
    fn a_verified_answer_marks_and_moves_the_stored_breakpoint() {
        let mut reg = Registry::default();
        reg.toggle_breakpoint(Path::new("/a.rs"), 10); // 0-based row 10
        // Adapter says verified but moved to wire line 12 (1-based) == row 11.
        reg.set_verified(
            Path::new("/a.rs"),
            &[crate::dap::types::Breakpoint { verified: true, line: Some(12), message: None }],
        );
        let bp = &reg.breakpoints_for(Path::new("/a.rs"))[0];
        assert!(bp.verified);
        assert_eq!(bp.moved_to, Some(11));
    }

    #[test]
    fn a_stopped_event_becomes_a_stopped_effect() {
        let fake = FakeSpawn::default();
        let mut reg = Registry::default();
        reg.set_spawner(fake.clone());

        let id = reg
            .launch("codelldb", &["codelldb".into()], Path::new("/proj"), "launch", json!({}))
            .expect("spawner is set");

        let seq = fake.last(id, "initialize").unwrap()["seq"].as_i64().unwrap();
        fake.respond(id, seq, "initialize", true, json!({}));
        assert!(reg.pump().is_empty(), "initialize answered, nothing for the editor yet");

        fake.event(id, "initialized", Value::Null);
        assert!(reg.pump().is_empty(), "no breakpoints — configurationDone fires on its own");
        assert!(
            fake.methods(id).contains(&"configurationDone".to_string()),
            "the empty-breakpoints signal fired the gate: {:?}",
            fake.methods(id)
        );

        fake.event(id, "stopped", json!({ "reason": "breakpoint", "threadId": 1 }));
        match reg.pump().as_slice() {
            [Effect::Stopped { session, thread: 1 }] => assert_eq!(*session, id),
            other => panic!("{other:?}"),
        }
    }
}
