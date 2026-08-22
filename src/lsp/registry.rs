//! The set of running clients, and the routing between buffers and servers.
//!
//! Clients are keyed `(server name, workspace root)`: one instance covers
//! every buffer of a project, and a second project in the same session gets
//! its own. The registry owns the [`Inbox`] the transports deliver into, and
//! [`Registry::accept`] turns each inbound message into client bookkeeping
//! plus, when the editor must act, an [`Effect`] — the registry never touches
//! editor state itself.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use ropey::Rope;
use serde_json::{Value, json};

use super::client::{Client, Intent, Phase};
use super::pos::{Encoding, uri_of};
use super::transport::Spawn;
use super::types::{PublishDiagnostics, ShowMessage, SyncKind, WorkDone};
use super::{Doc, Inbound, Inbox, ServerId, client, rpc, sync, types};
use crate::buffer::{BufferId, Edit};

/// One server's definition, from `[lsp.servers.<name>]` — built-in defaults
/// with the user's config merged over them. See `docs/specs/config.md` for
/// the patch semantics and `docs/specs/lsp.md` for the fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    pub enabled: bool,
    /// argv. Empty means "defined but unusable", which routing skips.
    pub command: Vec<String>,
    /// The filetype names `crate::syntax::filetype` produces.
    pub filetypes: Vec<String>,
    /// Root markers, tried on every ancestor of the file before the `.git`
    /// fallback.
    pub roots: Vec<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self { enabled: true, command: Vec::new(), filetypes: Vec::new(), roots: Vec::new() }
    }
}

/// What the editor must do about an inbound message. Everything else —
/// responses, server requests, progress, death — is client bookkeeping the
/// registry has already done by the time `accept` returns.
#[derive(Debug)]
pub enum Effect {
    /// `publishDiagnostics`, routed to the buffer whose document it names.
    /// Positions are still wire positions; the editor converts them against
    /// the rope it owns, under this encoding.
    Diagnostics {
        buffer: BufferId,
        version: Option<i32>,
        diagnostics: Vec<types::Diagnostic>,
        encoding: Encoding,
    },
    /// A line for the status bar — a crash, or a `showMessage` at error or
    /// warning severity.
    Status(String),
}

pub struct Registry {
    inbox: Inbox,
    /// How servers come to exist — supplied by the frontend, exactly as the
    /// clipboard is: spawning processes is a fact about the host, and an
    /// embedder that has none (a WASM sandbox, a test) supplies its own or
    /// nothing. `None` attaches nothing and `:lsp` says so.
    spawner: Option<Box<dyn Spawn>>,
    clients: Vec<Client>,
    next_server: u32,
    /// Spawn failures by `(server name, root)`, so a missing binary is tried
    /// once per project and not once per settle. `:lsp restart` clears it.
    failed: HashMap<(String, PathBuf), String>,
    /// URI → owner, for routing `publishDiagnostics`. The `ServerId` guards
    /// against a message from a replaced instance landing on its successor's
    /// document.
    docs: HashMap<String, (ServerId, BufferId)>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            inbox: Inbox::default(),
            spawner: None,
            clients: Vec::new(),
            next_server: 0,
            failed: HashMap::new(),
            docs: HashMap::new(),
        }
    }
}

impl Registry {
    /// The inbox transports deliver into — where the frontend's waker is
    /// registered, and where a test answers as the server.
    pub fn inbox(&self) -> &Inbox {
        &self.inbox
    }

    /// Replaces how transports come to exist — a test's fake, or a
    /// process-less embedding's own.
    pub fn set_spawner(&mut self, spawner: impl Spawn + 'static) {
        self.spawner = Some(Box::new(spawner));
    }

    /// Everything that has arrived, for the editor to feed through
    /// [`Registry::accept`].
    pub fn drain(&mut self) -> Vec<(ServerId, Inbound)> {
        self.inbox.drain()
    }

    pub fn instance(&self, id: ServerId) -> Option<&Client> {
        self.clients.iter().find(|c| c.id == id)
    }

    pub fn instance_mut(&mut self, id: ServerId) -> Option<&mut Client> {
        self.clients.iter_mut().find(|c| c.id == id)
    }

    /// Routes a buffer to a server: picks the config claiming its filetype,
    /// finds the workspace root, reuses or spawns the client, and registers
    /// the document. `Err` is the reason `:lsp` reports. `didOpen` is *not*
    /// sent here — it waits for the handshake, in [`Registry::try_open`].
    pub fn attach(
        &mut self,
        buffer: BufferId,
        path: &Path,
        filetype: &str,
        servers: &BTreeMap<String, ServerConfig>,
    ) -> Result<Doc, String> {
        let (name, config) = servers
            .iter()
            .find(|(_, s)| {
                s.enabled && !s.command.is_empty() && s.filetypes.iter().any(|f| f == filetype)
            })
            .ok_or_else(|| format!("no server for filetype {filetype}"))?;

        let abs = super::pos::canonical(path)?;
        let dir = abs.parent().unwrap_or(Path::new("/"));
        let root = find_root(dir, &config.roots).unwrap_or_else(|| dir.to_path_buf());

        let key = (name.clone(), root.clone());
        if let Some(reason) = self.failed.get(&key) {
            return Err(reason.clone());
        }

        let server = match self.clients.iter().find(|c| c.name == *name && c.root == root) {
            Some(c) if matches!(c.phase, Phase::Dead { .. }) => {
                return Err(format!("{name} exited — :lsp restart"));
            }
            Some(c) => c.id,
            None => {
                let Some(spawner) = self.spawner.as_deref() else {
                    return Err("this frontend supplies no server spawner".into());
                };
                let id = ServerId(self.next_server);
                self.next_server += 1;
                let started =
                    Client::start(id, name, &config.command, &root, self.inbox.clone(), spawner);
                match started {
                    Ok(client) => {
                        self.clients.push(client);
                        id
                    }
                    Err(reason) => {
                        self.failed.insert(key, reason.clone());
                        return Err(reason);
                    }
                }
            }
        };

        let uri = uri_of(&abs);
        self.docs.insert(uri.clone(), (server, buffer));
        Ok(Doc { server, uri, version: 0, opened: false, diagnostics: Vec::new() })
    }

    /// Sends `didOpen` once the client is Running. Waiting here rather than
    /// queueing at attach means the text sent is the text *now* — whatever
    /// was typed during the handshake rides along for free, and no queued
    /// incremental change can predate the server's first sight of the file.
    pub fn try_open(&mut self, doc: &mut Doc, filetype: &str, rope: &Rope) {
        if doc.opened {
            return;
        }
        let Some(client) = self.clients.iter_mut().find(|c| c.id == doc.server) else { return };
        if !client.running() {
            return;
        }
        doc.opened = true;
        doc.version = 1;
        client.notify(
            "textDocument/didOpen",
            json!({ "textDocument": {
                "uri": doc.uri, "languageId": filetype,
                "version": doc.version, "text": rope.to_string(),
            }}),
        );
    }

    /// One settle batch, as one `didChange` — composed line-granular for a
    /// server that wants incremental sync, the whole rope for one that wants
    /// full, nothing for one that asked for nothing.
    pub fn change(&mut self, doc: &mut Doc, rope: &Rope, edits: &[Edit]) {
        if !doc.opened || edits.is_empty() {
            return;
        }
        let Some(client) = self.clients.iter_mut().find(|c| c.id == doc.server) else { return };
        if !client.running() {
            return;
        }
        let changes = match client.sync.kind {
            SyncKind::None => return,
            SyncKind::Full => json!([{ "text": rope.to_string() }]),
            SyncKind::Incremental => {
                let Some(span) = sync::compose(edits) else { return };
                let (range, text) = sync::change(rope, span);
                json!([{ "range": range, "text": text }])
            }
        };
        doc.version += 1;
        client.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": doc.uri, "version": doc.version },
                "contentChanges": changes,
            }),
        );
    }

    /// `didSave`, when the server registered for it.
    pub fn saved(&mut self, doc: &Doc, rope: &Rope) {
        if !doc.opened {
            return;
        }
        let Some(client) = self.clients.iter_mut().find(|c| c.id == doc.server) else { return };
        let Some(include_text) = client.sync.save else { return };
        let mut params = json!({ "textDocument": { "uri": doc.uri } });
        if include_text {
            params["text"] = Value::String(rope.to_string());
        }
        client.notify("textDocument/didSave", params);
    }

    /// The buffer is going away, or its path changed under the document.
    pub fn close(&mut self, doc: &Doc) {
        self.docs.remove(&doc.uri);
        let Some(client) = self.clients.iter_mut().find(|c| c.id == doc.server) else { return };
        if doc.opened {
            client.notify("textDocument/didClose", json!({ "textDocument": { "uri": doc.uri } }));
        }
    }

    /// One inbound message: does the client bookkeeping, answers server
    /// requests, and returns what only the editor can do.
    pub fn accept(&mut self, from: ServerId, msg: Inbound) -> Option<Effect> {
        // A message from an id no client holds is a replaced instance's
        // leftovers, arrived after `:lsp restart` — history, not traffic.
        let client = self.clients.iter_mut().find(|c| c.id == from)?;

        match msg {
            Inbound::Response { id, result } => match client.take_intent(id)? {
                Intent::Initialize => match result {
                    Ok(value) => {
                        client.finish_initialize(value);
                        None
                    }
                    Err(e) => {
                        let name = client.name.clone();
                        client.die(format!("initialize failed: {}", e.message));
                        Some(Effect::Status(format!("{name}: initialize failed — :lsp")))
                    }
                },
                // The answer to `shutdown` needs no action — `exit` already
                // followed it out the door.
                Intent::Shutdown => None,
            },

            Inbound::Request { id, method, params } => {
                let response = match method.as_str() {
                    // Nothing to configure yet; a null per asked-for item is
                    // the spec's "no opinion".
                    "workspace/configuration" => {
                        let n = params["items"].as_array().map_or(0, Vec::len);
                        rpc::response_ok(&id, Value::Array(vec![Value::Null; n]))
                    }
                    // Acknowledged and ignored: bi does not do dynamic
                    // registration, and saying so would be `MethodNotFound`
                    // on a call every server makes politely.
                    "client/registerCapability" | "client/unregisterCapability" => {
                        rpc::response_ok(&id, Value::Null)
                    }
                    "window/workDoneProgress/create" => rpc::response_ok(&id, Value::Null),
                    // No UI to ask with; null is "the user chose nothing".
                    "window/showMessageRequest" => rpc::response_ok(&id, Value::Null),
                    "workspace/applyEdit" => rpc::response_ok(&id, json!({ "applied": false })),
                    _ => rpc::response_err(&id, rpc::METHOD_NOT_FOUND, &method),
                };
                client.respond(response);
                None
            }

            Inbound::Notification { method, params } => match method.as_str() {
                "textDocument/publishDiagnostics" => {
                    let parsed: PublishDiagnostics = serde_json::from_value(params).ok()?;
                    let encoding = client.encoding;
                    let &(owner, buffer) = self.docs.get(&parsed.uri)?;
                    // Guards a replaced instance's diagnostics landing on its
                    // successor's document.
                    (owner == from).then_some(Effect::Diagnostics {
                        buffer,
                        version: parsed.version,
                        diagnostics: parsed.diagnostics,
                        encoding,
                    })
                }
                "window/showMessage" => {
                    let parsed: ShowMessage = serde_json::from_value(params).ok()?;
                    // Error and warning reach the user; info and log are the
                    // server thinking out loud.
                    (parsed.typ <= 2)
                        .then(|| Effect::Status(format!("{}: {}", client.name, parsed.message)))
                }
                "$/progress" => {
                    let token = match &params["token"] {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    match serde_json::from_value(params["value"].clone()).ok()? {
                        WorkDone::Begin { title, message, percentage } => {
                            let progress = client::Progress { title, message, percentage };
                            client.progress.insert(token, progress);
                        }
                        WorkDone::Report { message, percentage } => {
                            if let Some(p) = client.progress.get_mut(&token) {
                                if message.is_some() {
                                    p.message = message;
                                }
                                if percentage.is_some() {
                                    p.percentage = percentage;
                                }
                            }
                        }
                        WorkDone::End { .. } => {
                            client.progress.remove(&token);
                        }
                    }
                    None
                }
                _ => None,
            },

            Inbound::Eof => {
                let reason = match client.exit_status() {
                    Some(code) => format!("exited (code {code})"),
                    None => "pipe closed".to_string(),
                };
                let name = client.name.clone();
                client.die(reason.clone());
                Some(Effect::Status(format!("{name}: {reason} — :lsp")))
            }
        }
    }

    /// Removes one instance for `:lsp restart` / `:lsp stop`: a quick
    /// shutdown, the spawn-failure slate wiped, its documents forgotten.
    /// Re-attaching afterwards is the caller's move.
    pub fn kill_instance(&mut self, id: ServerId) {
        let Some(at) = self.clients.iter().position(|c| c.id == id) else { return };
        let mut client = self.clients.remove(at);
        client.shutdown(Duration::from_millis(100));
        self.failed.remove(&(client.name.clone(), client.root.clone()));
        self.docs.retain(|_, &mut (server, _)| server != id);
    }

    /// Forgets recorded spawn failures, so `:lsp restart` after installing
    /// the missing binary actually retries.
    pub fn clear_failures(&mut self) {
        self.failed.clear();
    }

    /// Session end: every server asked to leave, briefly waited for, then
    /// killed. Sequential, but the patience is short and the count is small.
    pub fn shutdown_all(&mut self) {
        for client in &mut self.clients {
            client.shutdown(Duration::from_millis(200));
        }
    }
}

/// The nearest ancestor holding one of `markers`, else the nearest holding
/// `.git` — the universal project mark — else `None`, which the caller reads
/// as "the file's own directory".
fn find_root(dir: &Path, markers: &[String]) -> Option<PathBuf> {
    for dir in dir.ancestors() {
        if markers.iter().any(|m| dir.join(m).exists()) {
            return Some(dir.to_path_buf());
        }
    }
    dir.ancestors().find(|dir| dir.join(".git").exists()).map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use crate::buffer::{Buffer, Cursor};
    use crate::lsp::transport::fake::FakeSpawn;

    use super::*;

    /// A registry wired to a fake, one configured server, and a scratch
    /// project directory holding `Cargo.toml` and `src/main.rs`.
    struct Rig {
        registry: Registry,
        fake: FakeSpawn,
        servers: BTreeMap<String, ServerConfig>,
        dir: PathBuf,
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn rig(name: &str) -> Rig {
        let dir = std::env::temp_dir().join(format!("bi-lsp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();

        let fake = FakeSpawn::default();
        let mut registry = Registry::default();
        registry.set_spawner(fake.clone());

        let mut servers = BTreeMap::new();
        servers.insert(
            "rust-analyzer".to_string(),
            ServerConfig {
                enabled: true,
                command: vec!["rust-analyzer".into()],
                filetypes: vec!["rust".into()],
                roots: vec!["Cargo.toml".into()],
            },
        );
        Rig { registry, fake, servers, dir }
    }

    /// Attach, grant the handshake, pump, and open — the standard opening.
    fn opened(rig: &mut Rig, capabilities: Value) -> (Doc, Buffer) {
        let path = rig.dir.join("src/main.rs");
        let mut doc = rig
            .registry
            .attach(BufferId(0), &path, "rust", &rig.servers)
            .expect("a server is configured");
        let mut buffer = Buffer::open(&path).unwrap();
        buffer.pending_edits.clear();

        rig.fake.grant(doc.server, capabilities);
        for (from, msg) in rig.registry.drain() {
            rig.registry.accept(from, msg);
        }
        rig.registry.try_open(&mut doc, "rust", buffer.rope());
        (doc, buffer)
    }

    fn incremental() -> Value {
        json!({ "positionEncoding": "utf-8",
                "textDocumentSync": { "openClose": true, "change": 2, "save": true } })
    }

    #[test]
    fn attach_finds_the_marked_root_and_spawns_there() {
        let mut rig = rig("root");
        let path = rig.dir.join("src/main.rs");
        let doc = rig.registry.attach(BufferId(0), &path, "rust", &rig.servers).unwrap();

        let spawned = rig.fake.spawned.lock().unwrap();
        let (id, _, root) = &spawned[0];
        assert_eq!(*id, doc.server);
        // Canonical, because the URI is built from the canonical path too.
        assert_eq!(*root, rig.dir.canonicalize().unwrap(), "Cargo.toml marks the root");
        drop(spawned);

        assert_eq!(rig.fake.methods(doc.server), vec!["initialize"], "and nothing else yet");
        let init = rig.fake.last(doc.server, "initialize").unwrap();
        assert_eq!(init["params"]["capabilities"]["general"]["positionEncodings"][0], "utf-8");
    }

    #[test]
    fn no_config_claims_the_filetype() {
        let mut rig = rig("none");
        let err = rig
            .registry
            .attach(BufferId(0), &rig.dir.join("src/main.rs"), "markdown", &rig.servers.clone())
            .unwrap_err();
        assert!(err.contains("markdown"), "{err}");
    }

    #[test]
    fn did_open_waits_for_the_handshake_and_then_carries_the_text() {
        let mut rig = rig("open");
        let (doc, _) = opened(&mut rig, incremental());

        assert_eq!(
            rig.fake.methods(doc.server),
            vec!["initialize", "initialized", "textDocument/didOpen"],
            "the protocol's own order"
        );
        let open = rig.fake.last(doc.server, "textDocument/didOpen").unwrap();
        assert_eq!(open["params"]["textDocument"]["languageId"], "rust");
        assert_eq!(open["params"]["textDocument"]["version"], 1);
        assert_eq!(open["params"]["textDocument"]["text"], "fn main() {}\n");
        assert!(doc.opened);
    }

    #[test]
    fn before_the_handshake_nothing_opens() {
        let mut rig = rig("early");
        let path = rig.dir.join("src/main.rs");
        let mut doc = rig.registry.attach(BufferId(0), &path, "rust", &rig.servers).unwrap();
        let buffer = Buffer::open(&path).unwrap();

        rig.registry.try_open(&mut doc, "rust", buffer.rope());
        assert!(!doc.opened);
        assert_eq!(rig.fake.methods(doc.server), vec!["initialize"]);
    }

    #[test]
    fn a_change_composes_lines_and_bumps_the_version() {
        let mut rig = rig("change");
        let (mut doc, mut buffer) = opened(&mut rig, incremental());

        buffer.insert_str(Cursor::at(3), "x");
        let edits = std::mem::take(&mut buffer.pending_edits);
        rig.registry.change(&mut doc, buffer.rope(), &edits);

        assert_eq!(doc.version, 2);
        let change = rig.fake.last(doc.server, "textDocument/didChange").unwrap();
        let params = &change["params"];
        assert_eq!(params["textDocument"]["version"], 2);
        let c = &params["contentChanges"][0];
        assert_eq!(c["range"]["start"], json!({ "line": 0, "character": 0 }));
        assert_eq!(c["range"]["end"], json!({ "line": 1, "character": 0 }));
        assert_eq!(c["text"], "fn xmain() {}\n");
    }

    #[test]
    fn a_full_sync_server_gets_the_whole_rope_and_no_range() {
        let mut rig = rig("full");
        let (mut doc, mut buffer) = opened(&mut rig, json!({ "textDocumentSync": 1 }));

        buffer.insert_str(Cursor::at(0), "x");
        let edits = std::mem::take(&mut buffer.pending_edits);
        rig.registry.change(&mut doc, buffer.rope(), &edits);

        let c = &rig.fake.last(doc.server, "textDocument/didChange").unwrap()["params"]["contentChanges"]
            [0];
        assert_eq!(c["text"], "xfn main() {}\n");
        assert!(c.get("range").is_none());
    }

    #[test]
    fn a_server_that_asked_for_no_sync_gets_none() {
        let mut rig = rig("nosync");
        let (mut doc, mut buffer) = opened(&mut rig, json!({}));

        buffer.insert_str(Cursor::at(0), "x");
        let edits = std::mem::take(&mut buffer.pending_edits);
        rig.registry.change(&mut doc, buffer.rope(), &edits);

        assert_eq!(rig.fake.last(doc.server, "textDocument/didChange"), None);
        assert_eq!(doc.version, 1, "a version the server never heard of would poison the checks");
    }

    #[test]
    fn did_save_honours_the_capability_in_all_three_shapes() {
        // Wants the text.
        let mut r = rig("savetext");
        let caps = json!({ "textDocumentSync": { "change": 2, "save": { "includeText": true } } });
        let (doc, buffer) = opened(&mut r, caps);
        r.registry.saved(&doc, buffer.rope());
        let save = r.fake.last(doc.server, "textDocument/didSave").unwrap();
        assert_eq!(save["params"]["text"], "fn main() {}\n");

        // Wants the event only.
        let mut r = rig("saveplain");
        let (doc, buffer) = opened(&mut r, incremental());
        r.registry.saved(&doc, buffer.rope());
        let save = r.fake.last(doc.server, "textDocument/didSave").unwrap();
        assert!(save["params"].get("text").is_none());

        // Never asked.
        let mut r = rig("savenone");
        let (doc, buffer) = opened(&mut r, json!({ "textDocumentSync": { "change": 2 } }));
        r.registry.saved(&doc, buffer.rope());
        assert_eq!(r.fake.last(doc.server, "textDocument/didSave"), None);
    }

    #[test]
    fn diagnostics_route_to_the_buffer_with_the_granted_encoding() {
        let mut rig = rig("diag");
        let (doc, _) = opened(&mut rig, incremental());

        let params = json!({
            "uri": doc.uri, "version": 1,
            "diagnostics": [{ "range": { "start": { "line": 0, "character": 3 },
                                         "end": { "line": 0, "character": 7 } },
                              "severity": 1, "message": "nope" }]
        });
        let effect = rig
            .registry
            .accept(
                doc.server,
                Inbound::Notification { method: "textDocument/publishDiagnostics".into(), params },
            )
            .expect("routed");
        match effect {
            Effect::Diagnostics { buffer, version, diagnostics, encoding } => {
                assert_eq!(buffer, BufferId(0));
                assert_eq!(version, Some(1));
                assert_eq!(diagnostics.len(), 1);
                assert_eq!(encoding, Encoding::Utf8, "what initialize granted");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn diagnostics_for_an_unknown_uri_or_a_replaced_instance_drop() {
        let mut rig = rig("stale");
        let (doc, _) = opened(&mut rig, incremental());

        let unknown = json!({ "uri": "file:///elsewhere.rs", "diagnostics": [] });
        assert!(
            rig.registry
                .accept(
                    doc.server,
                    Inbound::Notification {
                        method: "textDocument/publishDiagnostics".into(),
                        params: unknown,
                    }
                )
                .is_none()
        );

        // A message from an id no client holds any more is history.
        rig.registry.kill_instance(doc.server);
        let late = json!({ "uri": doc.uri, "diagnostics": [] });
        assert!(
            rig.registry
                .accept(
                    doc.server,
                    Inbound::Notification {
                        method: "textDocument/publishDiagnostics".into(),
                        params: late,
                    }
                )
                .is_none()
        );
    }

    #[test]
    fn server_requests_are_answered_not_left_dangling() {
        let mut rig = rig("req");
        let (doc, _) = opened(&mut rig, incremental());

        let asked = json!({ "items": [{ "section": "rust-analyzer" }, { "section": "x" }] });
        rig.registry.accept(
            doc.server,
            Inbound::Request {
                id: json!(41),
                method: "workspace/configuration".into(),
                params: asked,
            },
        );
        let sent = rig.fake.sent.lock().unwrap();
        let answer = sent.iter().find(|(_, m)| m["id"] == json!(41)).map(|(_, m)| m.clone());
        drop(sent);
        assert_eq!(answer.unwrap()["result"], json!([null, null]), "a null per item");

        rig.registry.accept(
            doc.server,
            Inbound::Request { id: json!("s-1"), method: "made/up".into(), params: Value::Null },
        );
        let sent = rig.fake.sent.lock().unwrap();
        let answer = sent.iter().find(|(_, m)| m["id"] == json!("s-1")).map(|(_, m)| m.clone());
        assert_eq!(answer.unwrap()["error"]["code"], json!(rpc::METHOD_NOT_FOUND));
    }

    #[test]
    fn eof_is_a_death_notice_and_dead_servers_refuse_new_documents() {
        let mut rig = rig("eof");
        let (doc, _) = opened(&mut rig, incremental());

        match rig.registry.accept(doc.server, Inbound::Eof) {
            Some(Effect::Status(s)) => assert!(s.contains("rust-analyzer"), "{s}"),
            other => panic!("{other:?}"),
        }
        assert!(rig.fake.killed.lock().unwrap().contains(&doc.server), "reaped");

        let err = rig
            .registry
            .attach(BufferId(1), &rig.dir.join("src/main.rs"), "rust", &rig.servers.clone())
            .unwrap_err();
        assert!(err.contains(":lsp restart"), "{err}");
    }

    #[test]
    fn a_spawn_failure_is_recorded_once_and_cleared_on_demand() {
        let mut rig = rig("fail");
        rig.fake.fail = Some("rust-analyzer: not found".into());
        rig.registry.set_spawner(rig.fake.clone());

        let path = rig.dir.join("src/main.rs");
        let err = rig.registry.attach(BufferId(0), &path, "rust", &rig.servers).unwrap_err();
        assert!(err.contains("not found"), "{err}");
        // The second try answers from the record without spawning.
        rig.registry.attach(BufferId(0), &path, "rust", &rig.servers.clone()).unwrap_err();
        assert!(rig.fake.spawned.lock().unwrap().is_empty());

        // `:lsp restart` wipes the slate; with the binary "installed", it works.
        rig.registry.clear_failures();
        rig.fake.fail = None;
        rig.registry.set_spawner(rig.fake.clone());
        rig.registry.attach(BufferId(0), &path, "rust", &rig.servers.clone()).unwrap();
    }

    #[test]
    fn one_project_one_instance_and_progress_lands_on_it() {
        let mut rig = rig("share");
        let (doc, _) = opened(&mut rig, incremental());
        std::fs::write(rig.dir.join("src/lib.rs"), "").unwrap();
        let second = rig
            .registry
            .attach(BufferId(1), &rig.dir.join("src/lib.rs"), "rust", &rig.servers.clone())
            .unwrap();
        assert_eq!(second.server, doc.server, "same name, same root, same instance");

        for value in [
            json!({ "kind": "begin", "title": "indexing" }),
            json!({ "kind": "report", "percentage": 40 }),
        ] {
            rig.registry.accept(
                doc.server,
                Inbound::Notification {
                    method: "$/progress".into(),
                    params: json!({ "token": "t", "value": value }),
                },
            );
        }
        let progress = &rig.registry.instance(doc.server).unwrap().progress;
        assert_eq!(progress["t"].title, "indexing");
        assert_eq!(progress["t"].percentage, Some(40));

        rig.registry.accept(
            doc.server,
            Inbound::Notification {
                method: "$/progress".into(),
                params: json!({ "token": "t", "value": { "kind": "end" } }),
            },
        );
        assert!(rig.registry.instance(doc.server).unwrap().progress.is_empty());
    }

    #[test]
    fn show_message_reaches_the_status_line_only_when_it_matters() {
        let mut rig = rig("msg");
        let (doc, _) = opened(&mut rig, incremental());

        let error = json!({ "type": 1, "message": "cargo metadata failed" });
        match rig.registry.accept(
            doc.server,
            Inbound::Notification { method: "window/showMessage".into(), params: error },
        ) {
            Some(Effect::Status(s)) => assert_eq!(s, "rust-analyzer: cargo metadata failed"),
            other => panic!("{other:?}"),
        }

        let chatter = json!({ "type": 3, "message": "loading 1/2000" });
        assert!(
            rig.registry
                .accept(
                    doc.server,
                    Inbound::Notification { method: "window/showMessage".into(), params: chatter }
                )
                .is_none()
        );
    }

    #[test]
    fn close_tells_the_server_and_forgets_the_route() {
        let mut rig = rig("close");
        let (doc, _) = opened(&mut rig, incremental());

        rig.registry.close(&doc);
        assert!(rig.fake.last(doc.server, "textDocument/didClose").is_some());
        let params = json!({ "uri": doc.uri, "diagnostics": [] });
        assert!(
            rig.registry
                .accept(
                    doc.server,
                    Inbound::Notification {
                        method: "textDocument/publishDiagnostics".into(),
                        params,
                    }
                )
                .is_none()
        );
    }

    #[test]
    fn find_root_prefers_the_marker_then_git_then_gives_up() {
        let dir = std::env::temp_dir().join(format!("bi-lsp-roots-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("repo/inner/deep")).unwrap();
        std::fs::create_dir_all(dir.join("repo/.git")).unwrap();
        std::fs::write(dir.join("repo/inner/Cargo.toml"), "").unwrap();

        let deep = dir.join("repo/inner/deep");
        let markers = vec!["Cargo.toml".to_string()];
        assert_eq!(find_root(&deep, &markers), Some(dir.join("repo/inner")));
        // No marker anywhere: the repository is the project.
        assert_eq!(find_root(&deep, &[]), Some(dir.join("repo")));
        // Nothing at all: the caller falls back to the file's directory.
        std::fs::remove_dir_all(dir.join("repo/.git")).unwrap();
        std::fs::remove_file(dir.join("repo/inner/Cargo.toml")).unwrap();
        assert_eq!(find_root(&deep, &markers), None);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
