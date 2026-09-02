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
use crate::window::WindowId;

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
    /// argv that installs the server — the ecosystem's own one-liner, run by
    /// `:lsp install`. Empty means there is none. See
    /// `docs/specs/lsp-install.md`.
    pub install: Vec<String>,
    /// What `:lsp install` says when there is no one-liner — "clangd ships
    /// with LLVM", for the ecosystems where a sentence is the honest answer.
    pub install_hint: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            command: Vec::new(),
            filetypes: Vec::new(),
            roots: Vec::new(),
            install: Vec::new(),
            install_hint: String::new(),
        }
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
    /// A goto answer — definition, declaration or implementation, as `kind`
    /// says: where to jump `window`. Wire ranges still, since the target file
    /// may not even be open yet — the editor converts after loading it.
    Goto {
        kind: super::Goto,
        window: WindowId,
        targets: Vec<(PathBuf, types::Range)>,
        encoding: Encoding,
    },
    /// A references answer, bound for a `Results` pane rooted at the
    /// client's workspace root.
    References {
        symbol: String,
        root: PathBuf,
        targets: Vec<(PathBuf, types::Range)>,
        encoding: Encoding,
    },
    /// A formatting answer. `version` is what the request was computed
    /// against; the editor drops the lot on a mismatch.
    Formatting { buffer: BufferId, version: i32, edits: Vec<types::TextEdit>, encoding: Encoding },
    /// A hover answer, already normalised to one markdown string — `None`
    /// when the server had nothing to say.
    Hover { window: WindowId, anchor: usize, markdown: Option<String> },
    /// A completion answer. `incomplete` means the server wants re-asking as
    /// the word grows rather than local narrowing.
    Completion {
        buffer: BufferId,
        request: u64,
        manual: bool,
        incomplete: bool,
        items: Vec<types::CompletionItem>,
        encoding: Encoding,
    },
    /// A signature answer, already resolved to what the float draws — `None`
    /// is the server saying the cursor left the call, which means close.
    Signature { request: u64, help: Option<SignatureData> },
    /// A code actions answer: what the server offers at the range asked
    /// about, disabled entries already dropped, bare commands already
    /// normalised into [`types::CodeAction`]. `server` is who to send a
    /// chosen action's command back to.
    CodeActions {
        buffer: BufferId,
        version: i32,
        server: ServerId,
        actions: Vec<types::CodeAction>,
        encoding: Encoding,
    },
    /// A server→client `workspace/applyEdit` — the edits a command-backed
    /// action caused. Already answered `applied: true` on the wire; the
    /// editor applies it inside `settle` like every other effect.
    ApplyEdit { edit: types::WorkspaceEdit, encoding: Encoding },
    /// A `codeAction/resolve` answer: the chosen action again, its lazy
    /// halves filled in. `None` when the answer did not parse as an action.
    ResolvedAction {
        buffer: BufferId,
        version: i32,
        server: ServerId,
        action: Option<types::CodeAction>,
        encoding: Encoding,
    },
    /// A `textDocument/rename` answer: the workspace edit that performs it,
    /// gated on the version the rename was asked at.
    RenameEdit { buffer: BufferId, version: i32, edit: types::WorkspaceEdit, encoding: Encoding },
}

/// The parameters float's content: one label, the active parameter's char
/// range within it, and how many other signatures the server offered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureData {
    pub label: String,
    pub active: Option<std::ops::Range<usize>>,
    pub total: usize,
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

    /// Files a request with the intent its answer resolves. `Err` is the
    /// status line's to show — a server that is starting, dead, or gone.
    pub fn request(
        &mut self,
        server: ServerId,
        method: &str,
        params: Value,
        intent: Intent,
    ) -> Result<(), String> {
        let Some(client) = self.clients.iter_mut().find(|c| c.id == server) else {
            return Err("lsp: instance gone — :lsp restart".into());
        };
        match &client.phase {
            Phase::Running => {
                client.request(method, params, intent);
                Ok(())
            }
            Phase::Starting => Err(format!("{} is still starting", client.name)),
            Phase::Dead { .. } => Err(format!("{} exited — :lsp restart", client.name)),
        }
    }

    /// One of the goto family — `textDocument/definition`, `declaration` or
    /// `implementation` — for the symbol at `position`. One method because
    /// they are one request under three names; the kind is the whole of the
    /// difference.
    pub fn goto(
        &mut self,
        kind: super::Goto,
        doc: &Doc,
        position: types::Position,
        window: WindowId,
    ) -> Result<(), String> {
        self.request(
            doc.server,
            kind.method(),
            json!({ "textDocument": { "uri": doc.uri }, "position": position }),
            Intent::Goto { kind, window },
        )
    }

    /// `textDocument/references`. The declaration is included — a reference
    /// list that hides where the thing lives answers half the question.
    pub fn references(
        &mut self,
        doc: &Doc,
        position: types::Position,
        symbol: String,
    ) -> Result<(), String> {
        self.request(
            doc.server,
            "textDocument/references",
            json!({
                "textDocument": { "uri": doc.uri },
                "position": position,
                "context": { "includeDeclaration": true },
            }),
            Intent::References { symbol },
        )
    }

    /// `textDocument/formatting`, under the indentation options in force.
    pub fn formatting(
        &mut self,
        doc: &Doc,
        buffer: BufferId,
        tab_width: usize,
        expandtab: bool,
    ) -> Result<(), String> {
        self.request(
            doc.server,
            "textDocument/formatting",
            json!({
                "textDocument": { "uri": doc.uri },
                "options": { "tabSize": tab_width, "insertSpaces": expandtab },
            }),
            Intent::Formatting { buffer, version: doc.version },
        )
    }

    /// `textDocument/codeAction` over `range`, echoing back the stored
    /// diagnostics that overlap it — the context clangd wants its own
    /// diagnostics returned in before it offers their fixes.
    pub fn code_actions(
        &mut self,
        doc: &Doc,
        buffer: BufferId,
        range: types::Range,
        diagnostics: Vec<types::Diagnostic>,
    ) -> Result<(), String> {
        self.request(
            doc.server,
            "textDocument/codeAction",
            json!({
                "textDocument": { "uri": doc.uri },
                "range": range,
                "context": { "diagnostics": diagnostics },
            }),
            Intent::CodeActions { buffer, version: doc.version },
        )
    }

    /// `textDocument/rename` — the symbol at `position`, renamed to
    /// `new_name` everywhere. The answer is a `WorkspaceEdit`, applied
    /// exactly as a code action's is.
    pub fn rename(
        &mut self,
        doc: &Doc,
        buffer: BufferId,
        position: types::Position,
        new_name: &str,
    ) -> Result<(), String> {
        self.request(
            doc.server,
            "textDocument/rename",
            json!({ "textDocument": { "uri": doc.uri }, "position": position,
                    "newName": new_name }),
            Intent::Rename { buffer, version: doc.version },
        )
    }

    /// `codeAction/resolve` for an action whose edit the server kept lazy.
    /// The params are the action exactly as it arrived — `data` is the
    /// server's bookmark, and only the untouched original is guaranteed to
    /// be honoured.
    pub fn resolve_action(
        &mut self,
        server: ServerId,
        action: &types::CodeAction,
        buffer: BufferId,
        version: i32,
    ) -> Result<(), String> {
        self.request(
            server,
            "codeAction/resolve",
            action.raw.clone(),
            Intent::Resolve { buffer, version },
        )
    }

    /// `workspace/executeCommand` for a chosen action's command half. The
    /// arguments go back verbatim — they are the server's own, and opaque.
    pub fn execute_command(
        &mut self,
        server: ServerId,
        command: &types::CommandLit,
    ) -> Result<(), String> {
        self.request(
            server,
            "workspace/executeCommand",
            json!({ "command": command.command, "arguments": command.arguments }),
            Intent::Command,
        )
    }

    /// `textDocument/hover` at `position`. The anchor rides the intent.
    pub fn hover(
        &mut self,
        doc: &Doc,
        position: types::Position,
        window: WindowId,
        anchor: usize,
    ) -> Result<(), String> {
        self.request(
            doc.server,
            "textDocument/hover",
            json!({ "textDocument": { "uri": doc.uri }, "position": position }),
            Intent::Hover { window, anchor },
        )
    }

    /// `textDocument/completion`. `trigger` is the character that opened the
    /// menu when one did — the context the spec wants servers told about.
    pub fn completion(
        &mut self,
        doc: &Doc,
        position: types::Position,
        buffer: BufferId,
        request: u64,
        manual: bool,
        trigger: Option<char>,
    ) -> Result<(), String> {
        let context = match trigger {
            Some(c) => json!({ "triggerKind": 2, "triggerCharacter": c.to_string() }),
            None => json!({ "triggerKind": 1 }),
        };
        self.request(
            doc.server,
            "textDocument/completion",
            json!({ "textDocument": { "uri": doc.uri }, "position": position,
                    "context": context }),
            Intent::Completion { buffer, request, manual },
        )
    }

    /// `textDocument/signatureHelp`. `trigger` is the char that opened or
    /// moved the float, when one did.
    pub fn signature(
        &mut self,
        doc: &Doc,
        position: types::Position,
        request: u64,
        trigger: Option<char>,
    ) -> Result<(), String> {
        let context = match trigger {
            Some(c) => json!({ "triggerKind": 2, "triggerCharacter": c.to_string(),
                               "isRetrigger": false }),
            None => json!({ "triggerKind": 3, "isRetrigger": true }),
        };
        self.request(
            doc.server,
            "textDocument/signatureHelp",
            json!({ "textDocument": { "uri": doc.uri }, "position": position,
                    "context": context }),
            Intent::Signature { request },
        )
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
                Intent::Goto { kind, window } => match result {
                    Ok(value) => Some(Effect::Goto {
                        kind,
                        window,
                        targets: locations(value),
                        encoding: client.encoding,
                    }),
                    Err(e) => Some(Effect::Status(format!("{}: {}", kind.noun(), e.message))),
                },
                Intent::References { symbol } => match result {
                    Ok(value) => Some(Effect::References {
                        symbol,
                        root: client.root.clone(),
                        targets: locations(value),
                        encoding: client.encoding,
                    }),
                    Err(e) => Some(Effect::Status(format!("references: {}", e.message))),
                },
                Intent::CodeActions { buffer, version } => match result {
                    Ok(value) => Some(Effect::CodeActions {
                        buffer,
                        version,
                        server: from,
                        actions: actions_of(value),
                        encoding: client.encoding,
                    }),
                    Err(e) => Some(Effect::Status(format!("actions: {}", e.message))),
                },
                // The answer to `executeCommand` is usually null — the edits
                // arrive separately as `workspace/applyEdit` — so only an
                // error has anything to say.
                Intent::Command => match result {
                    Ok(_) => None,
                    Err(e) => Some(Effect::Status(format!("action: {}", e.message))),
                },
                Intent::Resolve { buffer, version } => match result {
                    Ok(value) => Some(Effect::ResolvedAction {
                        buffer,
                        version,
                        server: from,
                        action: action_of(value),
                        encoding: client.encoding,
                    }),
                    Err(e) => Some(Effect::Status(format!("actions: {}", e.message))),
                },
                Intent::Rename { buffer, version } => match result {
                    // Null is the spec's "nothing renameable here".
                    Ok(Value::Null) => Some(Effect::Status("rename: nothing at the cursor".into())),
                    Ok(value) => match serde_json::from_value::<types::WorkspaceEdit>(value) {
                        Ok(edit) => Some(Effect::RenameEdit {
                            buffer,
                            version,
                            edit,
                            encoding: client.encoding,
                        }),
                        Err(_) => Some(Effect::Status("rename: unreadable answer".into())),
                    },
                    Err(e) => Some(Effect::Status(format!("rename: {}", e.message))),
                },
                Intent::Formatting { buffer, version } => match result {
                    Ok(value) => Some(Effect::Formatting {
                        buffer,
                        version,
                        edits: serde_json::from_value(value).unwrap_or_default(),
                        encoding: client.encoding,
                    }),
                    Err(e) => Some(Effect::Status(format!("format: {}", e.message))),
                },
                Intent::Hover { window, anchor } => match result {
                    Ok(value) => {
                        Some(Effect::Hover { window, anchor, markdown: hover_markdown(&value) })
                    }
                    Err(e) => Some(Effect::Status(format!("hover: {}", e.message))),
                },
                Intent::Signature { request } => match result {
                    Ok(value) => Some(Effect::Signature {
                        request,
                        help: signature_data(&value, client.encoding),
                    }),
                    // The float is cosmetic; an error is a close, not a scold.
                    Err(_) => Some(Effect::Signature { request, help: None }),
                },
                Intent::Completion { buffer, request, manual } => match result {
                    Ok(value) => {
                        let (incomplete, items) = completion_items(value);
                        Some(Effect::Completion {
                            buffer,
                            request,
                            manual,
                            incomplete,
                            items,
                            encoding: client.encoding,
                        })
                    }
                    // Silent unless summoned: an error per keystroke is
                    // worse than no completion.
                    Err(e) => manual.then(|| Effect::Status(format!("completion: {}", e.message))),
                },
            },

            Inbound::Request { id, method, params } => {
                let mut effect = None;
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
                    // The edits a command-backed code action caused. Answered
                    // `applied` optimistically — the effect is handed to the
                    // editor in the same pump, and a reply held back until it
                    // ran would mean holding the whole inbox.
                    "workspace/applyEdit" => {
                        match serde_json::from_value::<types::WorkspaceEdit>(params["edit"].clone())
                        {
                            Ok(edit) => {
                                effect =
                                    Some(Effect::ApplyEdit { edit, encoding: client.encoding });
                                rpc::response_ok(&id, json!({ "applied": true }))
                            }
                            Err(_) => rpc::response_ok(&id, json!({ "applied": false })),
                        }
                    }
                    _ => rpc::response_err(&id, rpc::METHOD_NOT_FOUND, &method),
                };
                client.respond(response);
                effect
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

/// Every `(path, range)` a definition or references answer names, whatever
/// wire shape it arrived in: null, one `Location`, an array of them, or an
/// array of `LocationLink`s. Non-`file://` URIs are dropped — they name
/// nothing on this filesystem.
fn locations(value: Value) -> Vec<(PathBuf, types::Range)> {
    fn one(v: &Value) -> Option<(PathBuf, types::Range)> {
        if v.get("uri").is_some() {
            let l: types::Location = serde_json::from_value(v.clone()).ok()?;
            Some((super::pos::path_of(&l.uri)?, l.range))
        } else {
            // A link's selection range is the symbol itself — where a jump
            // wants to land — rather than the whole declaration block.
            let l: types::LocationLink = serde_json::from_value(v.clone()).ok()?;
            Some((super::pos::path_of(&l.target_uri)?, l.target_selection_range))
        }
    }
    match value {
        Value::Array(items) => items.iter().filter_map(one).collect(),
        v @ Value::Object(_) => one(&v).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// A hover answer's `contents`, in any of its four wire shapes, as one
/// markdown string — or `None` for a null answer or an empty one.
///
/// A `MarkedString` with a language becomes a fenced block, which is what it
/// abbreviates; an array joins with blank lines.
fn hover_markdown(value: &Value) -> Option<String> {
    fn marked(v: &Value) -> Option<String> {
        match v {
            Value::String(s) => Some(s.clone()),
            Value::Object(o) => match (o.get("language").and_then(Value::as_str), o.get("value")) {
                (Some(language), Some(Value::String(value))) => {
                    Some(format!("```{language}\n{value}\n```"))
                }
                (None, Some(Value::String(value))) => Some(value.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    let contents = value.get("contents")?;
    let text = match contents {
        Value::Array(items) => items.iter().filter_map(marked).collect::<Vec<_>>().join("\n\n"),
        one => marked(one)?,
    };
    (!text.trim().is_empty()).then_some(text)
}

/// A signature answer as the float's content: the active signature's label,
/// the active parameter resolved to a char range in it, and the count. `None`
/// for null or empty — the server saying the cursor is not in a call.
///
/// The parameter's span arrives in two wire shapes — a substring of the
/// label, or a pair of offsets in the negotiated encoding — and both resolve
/// here, at the boundary, like every other wire position.
fn signature_data(value: &Value, encoding: Encoding) -> Option<SignatureData> {
    let signatures = value.get("signatures")?.as_array()?;
    if signatures.is_empty() {
        return None;
    }
    let active_signature =
        value.get("activeSignature").and_then(Value::as_u64).unwrap_or(0) as usize;
    let sig = signatures.get(active_signature).unwrap_or(&signatures[0]);
    let label = sig.get("label")?.as_str()?.to_string();

    // 3.16 lets each signature carry its own active parameter; the top-level
    // one is the fallback, and 0 is the spec's own default.
    let param_index = sig
        .get("activeParameter")
        .or_else(|| value.get("activeParameter"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let active = sig
        .get("parameters")
        .and_then(Value::as_array)
        .and_then(|params| params.get(param_index))
        .and_then(|param| param.get("label"))
        .and_then(|span| match span {
            Value::String(text) => {
                let start = label.find(text.as_str())?;
                let chars_before = label[..start].chars().count();
                Some(chars_before..chars_before + text.chars().count())
            }
            Value::Array(pair) => {
                let (a, b) = (pair.first()?.as_u64()?, pair.get(1)?.as_u64()?);
                Some(
                    super::pos::col_to_char(&label, a as u32, encoding)
                        ..super::pos::col_to_char(&label, b as u32, encoding),
                )
            }
            _ => None,
        });
    Some(SignatureData { label, active, total: signatures.len() })
}

/// A completion answer in either wire shape — a bare array, or a
/// `CompletionList` carrying `isIncomplete` — as `(incomplete, items)`.
/// A code actions answer as one list of [`types::CodeAction`].
///
/// The wire allows `(Command | CodeAction)[] | null`. A bare `Command` — its
/// `command` field is a string where a `CodeAction`'s is an object — becomes
/// an action with only the command half; a disabled action is dropped here,
/// because a row that cannot be chosen is a row that should not be offered.
fn actions_of(value: Value) -> Vec<types::CodeAction> {
    let Value::Array(items) = value else { return Vec::new() };
    items.into_iter().filter_map(action_of).filter(|action| action.disabled.is_none()).collect()
}

/// One `Command | CodeAction`, keeping the wire shape in `raw` — a later
/// `codeAction/resolve` must send back exactly what arrived.
fn action_of(item: Value) -> Option<types::CodeAction> {
    if item["command"].is_string() {
        let command: types::CommandLit = serde_json::from_value(item.clone()).ok()?;
        return Some(types::CodeAction {
            title: command.title.clone(),
            edit: None,
            command: Some(command),
            disabled: None,
            raw: item,
        });
    }
    let mut action: types::CodeAction = serde_json::from_value(item.clone()).ok()?;
    action.raw = item;
    Some(action)
}

fn completion_items(value: Value) -> (bool, Vec<types::CompletionItem>) {
    match value {
        Value::Array(_) => (false, serde_json::from_value(value).unwrap_or_default()),
        Value::Object(mut o) => {
            let incomplete = o.get("isIncomplete").and_then(Value::as_bool).unwrap_or(false);
            let items = o
                .remove("items")
                .map(|items| serde_json::from_value(items).unwrap_or_default())
                .unwrap_or_default();
            (incomplete, items)
        }
        _ => (false, Vec::new()),
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
    use crate::lsp::Goto;
    use crate::lsp::transport::fake::FakeSpawn;
    use crate::lsp::types::Position;

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
                ..ServerConfig::default()
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
    fn a_request_resolves_its_intent_into_an_effect() {
        let mut rig = rig("req-def");
        let (doc, _) = opened(&mut rig, incremental());
        let window = WindowId(0);

        rig.registry
            .request(
                doc.server,
                "textDocument/definition",
                json!({}),
                Intent::Goto { kind: Goto::Definition, window },
            )
            .unwrap();
        let sent = rig.fake.last(doc.server, "textDocument/definition").unwrap();
        let id = sent["id"].as_i64().unwrap();

        // The three wire shapes: a single Location…
        let single = json!({ "uri": "file:///a.rs",
            "range": { "start": { "line": 3, "character": 4 },
                       "end": { "line": 3, "character": 9 } } });
        let inbox = rig.fake.spawned.lock().unwrap()[0].1.clone();
        inbox.deliver(doc.server, Inbound::Response { id, result: Ok(single) });
        let (from, msg) = rig.registry.drain().pop().unwrap();
        match rig.registry.accept(from, msg) {
            Some(Effect::Goto { window: w, targets, encoding, .. }) => {
                assert_eq!(w, window);
                assert_eq!(targets[0].0, PathBuf::from("/a.rs"));
                assert_eq!(targets[0].1.start.line, 3);
                assert_eq!(encoding, Encoding::Utf8);
            }
            other => panic!("{other:?}"),
        }

        // …an array of LocationLinks…
        rig.registry
            .request(
                doc.server,
                "textDocument/definition",
                json!({}),
                Intent::Goto { kind: Goto::Definition, window },
            )
            .unwrap();
        let id =
            rig.fake.last(doc.server, "textDocument/definition").unwrap()["id"].as_i64().unwrap();
        let links = json!([{
            "targetUri": "file:///b.rs",
            "targetRange": { "start": { "line": 0, "character": 0 },
                             "end": { "line": 9, "character": 0 } },
            "targetSelectionRange": { "start": { "line": 2, "character": 7 },
                                      "end": { "line": 2, "character": 12 } }
        }, {
            "targetUri": "untitled:nope",
            "targetRange": { "start": { "line": 0, "character": 0 },
                             "end": { "line": 0, "character": 0 } },
            "targetSelectionRange": { "start": { "line": 0, "character": 0 },
                                      "end": { "line": 0, "character": 0 } }
        }]);
        inbox.deliver(doc.server, Inbound::Response { id, result: Ok(links) });
        let (from, msg) = rig.registry.drain().pop().unwrap();
        match rig.registry.accept(from, msg) {
            Some(Effect::Goto { targets, .. }) => {
                assert_eq!(targets.len(), 1, "the foreign scheme dropped");
                assert_eq!(targets[0].1.start, Position { line: 2, character: 7 }, "the symbol");
            }
            other => panic!("{other:?}"),
        }

        // …and null, which is an answer too.
        rig.registry
            .request(
                doc.server,
                "textDocument/definition",
                json!({}),
                Intent::Goto { kind: Goto::Definition, window },
            )
            .unwrap();
        let id =
            rig.fake.last(doc.server, "textDocument/definition").unwrap()["id"].as_i64().unwrap();
        inbox.deliver(doc.server, Inbound::Response { id, result: Ok(Value::Null) });
        let (from, msg) = rig.registry.drain().pop().unwrap();
        match rig.registry.accept(from, msg) {
            Some(Effect::Goto { targets, .. }) => assert!(targets.is_empty()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_goto_kind_picks_the_method_and_survives_to_the_effect() {
        let mut rig = rig("req-goto");
        let (doc, _) = opened(&mut rig, incremental());
        let window = WindowId(0);
        let position = Position { line: 1, character: 2 };

        for kind in [Goto::Definition, Goto::Declaration, Goto::Implementation] {
            rig.registry.goto(kind, &doc, position, window).unwrap();
            let sent = rig.fake.last(doc.server, kind.method()).unwrap();
            assert_eq!(sent["params"]["position"]["line"], 1);
            let id = sent["id"].as_i64().unwrap();

            let inbox = rig.fake.spawned.lock().unwrap()[0].1.clone();
            inbox.deliver(doc.server, Inbound::Response { id, result: Ok(Value::Null) });
            let (from, msg) = rig.registry.drain().pop().unwrap();
            match rig.registry.accept(from, msg) {
                Some(Effect::Goto { kind: k, targets, .. }) => {
                    assert_eq!(k, kind);
                    assert!(targets.is_empty());
                }
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn a_request_to_a_server_that_cannot_take_it_is_a_status_not_a_hang() {
        let mut rig = rig("req-early");
        let path = rig.dir.join("src/main.rs");
        let doc = rig.registry.attach(BufferId(0), &path, "rust", &rig.servers).unwrap();

        // Still starting.
        let err = rig
            .registry
            .request(
                doc.server,
                "textDocument/definition",
                json!({}),
                Intent::Goto { kind: Goto::Definition, window: WindowId(0) },
            )
            .unwrap_err();
        assert!(err.contains("starting"), "{err}");

        // Dead.
        rig.registry.accept(doc.server, Inbound::Eof);
        let err = rig
            .registry
            .request(
                doc.server,
                "textDocument/definition",
                json!({}),
                Intent::Goto { kind: Goto::Definition, window: WindowId(0) },
            )
            .unwrap_err();
        assert!(err.contains(":lsp restart"), "{err}");
    }

    #[test]
    fn a_formatting_answer_carries_the_version_it_was_asked_at() {
        let mut rig = rig("req-fmt");
        let (doc, _) = opened(&mut rig, incremental());

        rig.registry
            .request(
                doc.server,
                "textDocument/formatting",
                json!({}),
                Intent::Formatting { buffer: BufferId(0), version: 1 },
            )
            .unwrap();
        let id =
            rig.fake.last(doc.server, "textDocument/formatting").unwrap()["id"].as_i64().unwrap();
        let edits = json!([{ "range": { "start": { "line": 0, "character": 0 },
                                        "end": { "line": 0, "character": 2 } },
                             "newText": "    " }]);
        let inbox = rig.fake.spawned.lock().unwrap()[0].1.clone();
        inbox.deliver(doc.server, Inbound::Response { id, result: Ok(edits) });
        let (from, msg) = rig.registry.drain().pop().unwrap();
        match rig.registry.accept(from, msg) {
            Some(Effect::Formatting { buffer, version, edits, .. }) => {
                assert_eq!(buffer, BufferId(0));
                assert_eq!(version, 1);
                assert_eq!(edits[0].new_text, "    ");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn hover_contents_normalise_from_every_wire_shape() {
        // A bare string.
        let s = hover_markdown(&json!({ "contents": "plain words" }));
        assert_eq!(s.as_deref(), Some("plain words"));

        // MarkupContent.
        let m = hover_markdown(&json!({ "contents": { "kind": "markdown", "value": "# doc" } }));
        assert_eq!(m.as_deref(), Some("# doc"));

        // A MarkedString with a language becomes the fence it abbreviates.
        let l = hover_markdown(&json!({ "contents": { "language": "rust", "value": "fn f()" } }));
        assert_eq!(l.as_deref(), Some("```rust\nfn f()\n```"));

        // An array joins; an empty answer is no answer.
        let a = hover_markdown(&json!({ "contents": ["one", { "language": "c", "value": "x;" }] }));
        assert_eq!(a.as_deref(), Some("one\n\n```c\nx;\n```"));
        assert_eq!(hover_markdown(&json!({ "contents": "" })), None);
        assert_eq!(hover_markdown(&Value::Null), None);
    }

    #[test]
    fn completion_answers_parse_both_wire_shapes() {
        let bare = completion_items(json!([{ "label": "a" }, { "label": "b" }]));
        assert!(!bare.0);
        assert_eq!(bare.1.len(), 2);

        let list = completion_items(json!({
            "isIncomplete": true,
            "items": [{ "label": "c", "insertText": "c()", "kind": 3 }]
        }));
        assert!(list.0);
        assert_eq!(list.1[0].new_text(), "c()");

        assert_eq!(completion_items(Value::Null).1.len(), 0);
    }

    #[test]
    fn a_completion_request_carries_its_trigger_context() {
        let mut rig = rig("ctx");
        let (doc, _) = opened(&mut rig, incremental());
        let position = Position { line: 0, character: 3 };

        rig.registry.completion(&doc, position, BufferId(0), 1, false, Some('.')).unwrap();
        let sent = rig.fake.last(doc.server, "textDocument/completion").unwrap();
        assert_eq!(sent["params"]["context"]["triggerKind"], 2);
        assert_eq!(sent["params"]["context"]["triggerCharacter"], ".");

        rig.registry.completion(&doc, position, BufferId(0), 2, true, None).unwrap();
        let sent = rig.fake.last(doc.server, "textDocument/completion").unwrap();
        assert_eq!(sent["params"]["context"]["triggerKind"], 1);
    }

    #[test]
    fn a_failed_completion_is_silent_unless_summoned() {
        let mut rig = rig("quiet");
        let (doc, _) = opened(&mut rig, incremental());
        let inbox = rig.fake.spawned.lock().unwrap()[0].1.clone();
        let position = Position { line: 0, character: 0 };

        for (request, manual, expects_status) in [(1u64, false, false), (2, true, true)] {
            rig.registry.completion(&doc, position, BufferId(0), request, manual, None).unwrap();
            let id = rig.fake.last(doc.server, "textDocument/completion").unwrap()["id"]
                .as_i64()
                .unwrap();
            let error = super::super::rpc::ResponseError { code: 1, message: "busy".into() };
            inbox.deliver(doc.server, Inbound::Response { id, result: Err(error) });
            let (from, msg) = rig.registry.drain().pop().unwrap();
            let effect = rig.registry.accept(from, msg);
            assert_eq!(
                matches!(effect, Some(Effect::Status(_))),
                expects_status,
                "manual={manual}"
            );
        }
    }

    #[test]
    fn signature_answers_resolve_both_parameter_shapes() {
        // Offsets into the label, in the negotiated encoding.
        let offsets = json!({
            "signatures": [{
                "label": "fn add(a: i32, b: i32)",
                "parameters": [{ "label": [7, 13] }, { "label": [15, 21] }],
            }],
            "activeParameter": 1,
        });
        let data = signature_data(&offsets, Encoding::Utf8).unwrap();
        assert_eq!(data.label, "fn add(a: i32, b: i32)");
        assert_eq!(data.active, Some(15..21), "the second parameter");
        assert_eq!(data.total, 1);

        // A substring of the label — the other legal spelling.
        let substring = json!({
            "signatures": [
                { "label": "f(x: u8)", "parameters": [{ "label": "x: u8" }] },
                { "label": "f()" },
            ],
        });
        let data = signature_data(&substring, Encoding::Utf16).unwrap();
        assert_eq!(data.active, Some(2..7));
        assert_eq!(data.total, 2, "counted, not paged");

        // The per-signature activeParameter (3.16) outranks the top-level.
        let per_sig = json!({
            "signatures": [{
                "label": "g(a, b)",
                "activeParameter": 1,
                "parameters": [{ "label": "a" }, { "label": "b" }],
            }],
            "activeParameter": 0,
        });
        assert_eq!(signature_data(&per_sig, Encoding::Utf8).unwrap().active, Some(5..6));

        // Null and empty both mean "not in a call" — close.
        assert_eq!(signature_data(&Value::Null, Encoding::Utf8), None);
        assert_eq!(signature_data(&json!({ "signatures": [] }), Encoding::Utf8), None);
        // No parameters is a label with nothing to highlight, not a close.
        let bare = json!({ "signatures": [{ "label": "h()" }] });
        assert_eq!(signature_data(&bare, Encoding::Utf8).unwrap().active, None);
    }

    #[test]
    fn a_signature_request_says_what_moved_it() {
        let mut rig = rig("sigctx");
        let (doc, _) = opened(&mut rig, incremental());
        let position = Position { line: 0, character: 5 };

        rig.registry.signature(&doc, position, 1, Some('(')).unwrap();
        let sent = rig.fake.last(doc.server, "textDocument/signatureHelp").unwrap();
        assert_eq!(sent["params"]["context"]["triggerKind"], 2);
        assert_eq!(sent["params"]["context"]["triggerCharacter"], "(");

        rig.registry.signature(&doc, position, 2, None).unwrap();
        let sent = rig.fake.last(doc.server, "textDocument/signatureHelp").unwrap();
        assert_eq!(sent["params"]["context"]["triggerKind"], 3);
        assert_eq!(sent["params"]["context"]["isRetrigger"], true);
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
