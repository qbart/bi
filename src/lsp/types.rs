//! The protocol structs bi actually uses — a dozen, of a protocol with
//! hundreds. Owned rather than imported (the `lsp-types` crate) so each is
//! exactly the shape bi needs; a later feature adds its own few beside these.
//!
//! Everything here is wire shape. Positions are `u32` line/character pairs in
//! whatever encoding was negotiated — the conversion to bi's offsets lives in
//! `pos.rs`, and nothing outside `lsp/` should ever hold one of these.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

/// `textDocument/publishDiagnostics`, the one thing every server pushes
/// unasked.
#[derive(Debug, Clone, Deserialize)]
pub struct PublishDiagnostics {
    pub uri: String,
    /// The document version the diagnostics were computed against, when the
    /// server honours `versionSupport`. A mismatch means they describe text
    /// that no longer exists and their successor is already being computed.
    #[serde(default)]
    pub version: Option<i32>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Diagnostic {
    pub range: Range,
    /// 1 error, 2 warning, 3 info, 4 hint. Optional on the wire.
    #[serde(default)]
    pub severity: Option<u8>,
    #[serde(default)]
    pub source: Option<String>,
    pub message: String,
}

/// One `Location` from a definition or references answer.
#[derive(Debug, Clone, Deserialize)]
pub struct Location {
    pub uri: String,
    pub range: Range,
}

/// A `LocationLink` — the other shape a definition answer may take.
/// `target_selection_range` is the symbol itself, which is where a jump
/// wants to land; `target_range` is the whole declaration.
#[derive(Debug, Clone, Deserialize)]
pub struct LocationLink {
    #[serde(rename = "targetUri")]
    pub target_uri: String,
    #[serde(rename = "targetSelectionRange")]
    pub target_selection_range: Range,
}

/// One edit from `textDocument/formatting`.
#[derive(Debug, Clone, Deserialize)]
pub struct TextEdit {
    pub range: Range,
    #[serde(rename = "newText")]
    pub new_text: String,
}

/// What `initialize` answered with, reduced to the parts core reads.
/// `#[serde(default)]` throughout: a missing capability is an answer, not a
/// parse failure.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct InitializeResult {
    #[serde(default)]
    pub capabilities: Capabilities,
    #[serde(default, rename = "serverInfo")]
    pub server_info: Option<ServerInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Capabilities {
    /// "utf-8" when the server granted what bi asked for; absent means the
    /// mandated default, utf-16.
    #[serde(default, rename = "positionEncoding")]
    pub position_encoding: Option<String>,
    /// A bare number or an options object — see [`SyncCaps::parse`].
    #[serde(default, rename = "textDocumentSync")]
    pub text_document_sync: Option<Value>,
    /// Each provider is a bool or an options object; [`truthy`] reads both.
    #[serde(default, rename = "definitionProvider")]
    pub definition_provider: Option<Value>,
    #[serde(default, rename = "referencesProvider")]
    pub references_provider: Option<Value>,
    #[serde(default, rename = "documentFormattingProvider")]
    pub formatting_provider: Option<Value>,
}

/// Whether a `*Provider` capability is on. The spec lets a server say `true`,
/// `false`, or an options object — an object is an elaborate yes.
pub fn truthy(capability: Option<&Value>) -> bool {
    match capability {
        None | Some(Value::Bool(false)) | Some(Value::Null) => false,
        Some(_) => true,
    }
}

/// How the server wants document changes, decoded from either wire shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncCaps {
    pub kind: SyncKind,
    /// `Some` when `didSave` is wanted; the bool is `includeText`.
    pub save: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncKind {
    None,
    Full,
    Incremental,
}

impl Default for SyncCaps {
    /// A server that says nothing gets nothing — `TextDocumentSyncKind.None`
    /// is the spec's own default.
    fn default() -> Self {
        Self { kind: SyncKind::None, save: None }
    }
}

impl SyncCaps {
    pub fn parse(value: Option<&Value>) -> Self {
        let kind = |n: Option<i64>| match n {
            Some(1) => SyncKind::Full,
            Some(2) => SyncKind::Incremental,
            _ => SyncKind::None,
        };
        match value {
            // The number shorthand predates the options object and says
            // nothing about saving; sending a plain `didSave` anyway is what
            // its era of servers expects.
            Some(Value::Number(n)) => Self { kind: kind(n.as_i64()), save: Some(false) },
            Some(Value::Object(o)) => Self {
                kind: kind(o.get("change").and_then(Value::as_i64)),
                save: match o.get("save") {
                    Some(Value::Bool(true)) => Some(false),
                    Some(Value::Object(s)) => {
                        Some(s.get("includeText").and_then(Value::as_bool).unwrap_or(false))
                    }
                    _ => None,
                },
            },
            _ => Self::default(),
        }
    }
}

/// `window/showMessage` / `window/logMessage`: `typ` 1 error, 2 warning,
/// 3 info, 4 log.
#[derive(Debug, Clone, Deserialize)]
pub struct ShowMessage {
    #[serde(rename = "type")]
    pub typ: u8,
    pub message: String,
}

/// The `value` of a `$/progress` notification.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum WorkDone {
    Begin {
        title: String,
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        percentage: Option<u32>,
    },
    Report {
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        percentage: Option<u32>,
    },
    End {
        #[serde(default)]
        message: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn publish_diagnostics_parses_the_shape_rust_analyzer_sends() {
        // Trimmed from a captured rust-analyzer session: extra fields
        // (`code`, `codeDescription`, `tags`, `relatedInformation`) must be
        // ignored, not fatal.
        let params = json!({
            "uri": "file:///home/user/p/src/main.rs",
            "version": 3,
            "diagnostics": [{
                "range": { "start": { "line": 1, "character": 4 },
                           "end": { "line": 1, "character": 9 } },
                "severity": 1,
                "code": "E0425",
                "codeDescription": { "href": "https://doc.rust-lang.org/error_codes/E0425.html" },
                "source": "rustc",
                "message": "cannot find value `nope` in this scope",
                "relatedInformation": [],
                "tags": []
            }]
        });
        let parsed: PublishDiagnostics = serde_json::from_value(params).unwrap();
        assert_eq!(parsed.version, Some(3));
        assert_eq!(parsed.diagnostics.len(), 1);
        let d = &parsed.diagnostics[0];
        assert_eq!(d.severity, Some(1));
        assert_eq!(d.source.as_deref(), Some("rustc"));
        assert_eq!(d.range.start, Position { line: 1, character: 4 });
    }

    #[test]
    fn an_empty_publish_clears_and_a_versionless_one_parses() {
        let params = json!({ "uri": "file:///x", "diagnostics": [] });
        let parsed: PublishDiagnostics = serde_json::from_value(params).unwrap();
        assert_eq!(parsed.version, None);
        assert!(parsed.diagnostics.is_empty());
    }

    #[test]
    fn initialize_result_reads_the_two_capabilities_core_uses() {
        let result = json!({
            "capabilities": {
                "positionEncoding": "utf-8",
                "textDocumentSync": { "openClose": true, "change": 2,
                                      "save": { "includeText": false } },
                "hoverProvider": true,
                "completionProvider": { "triggerCharacters": [".", ":"] }
            },
            "serverInfo": { "name": "rust-analyzer", "version": "1.80.0" }
        });
        let parsed: InitializeResult = serde_json::from_value(result).unwrap();
        assert_eq!(parsed.capabilities.position_encoding.as_deref(), Some("utf-8"));
        let sync = SyncCaps::parse(parsed.capabilities.text_document_sync.as_ref());
        assert_eq!(sync, SyncCaps { kind: SyncKind::Incremental, save: Some(false) });
        assert_eq!(parsed.server_info.unwrap().name, "rust-analyzer");
    }

    #[test]
    fn an_empty_initialize_result_is_capabilities_that_ask_for_nothing() {
        let parsed: InitializeResult = serde_json::from_value(json!({})).unwrap();
        assert_eq!(parsed.capabilities.position_encoding, None);
        assert_eq!(
            SyncCaps::parse(parsed.capabilities.text_document_sync.as_ref()),
            SyncCaps::default()
        );
    }

    #[test]
    fn sync_caps_decode_every_wire_shape() {
        // The number shorthand.
        let n = SyncCaps::parse(Some(&json!(1)));
        assert_eq!(n, SyncCaps { kind: SyncKind::Full, save: Some(false) });

        // `save: true` is `includeText: false` said shorter.
        let b = SyncCaps::parse(Some(&json!({ "change": 2, "save": true })));
        assert_eq!(b, SyncCaps { kind: SyncKind::Incremental, save: Some(false) });

        // `includeText: true` wants the text in `didSave`.
        let t = SyncCaps::parse(Some(&json!({ "change": 1, "save": { "includeText": true } })));
        assert_eq!(t, SyncCaps { kind: SyncKind::Full, save: Some(true) });

        // An object that never mentions save wants no didSave at all.
        let none = SyncCaps::parse(Some(&json!({ "change": 2 })));
        assert_eq!(none, SyncCaps { kind: SyncKind::Incremental, save: None });

        assert_eq!(SyncCaps::parse(None), SyncCaps::default());
    }

    #[test]
    fn provider_capabilities_read_bool_and_object_alike() {
        let caps = json!({
            "definitionProvider": true,
            "referencesProvider": { "workDoneProgress": true },
            "documentFormattingProvider": false,
        });
        let parsed: Capabilities = serde_json::from_value(caps).unwrap();
        assert!(truthy(parsed.definition_provider.as_ref()), "a plain yes");
        assert!(truthy(parsed.references_provider.as_ref()), "an elaborate yes");
        assert!(!truthy(parsed.formatting_provider.as_ref()), "an explicit no");
        assert!(!truthy(None), "silence is no");
    }

    #[test]
    fn locations_links_and_edits_parse_their_wire_shapes() {
        let location: Location = serde_json::from_value(json!({
            "uri": "file:///a.rs",
            "range": { "start": { "line": 1, "character": 2 },
                       "end": { "line": 1, "character": 5 } }
        }))
        .unwrap();
        assert_eq!(location.range.start.line, 1);

        let link: LocationLink = serde_json::from_value(json!({
            "originSelectionRange": { "start": { "line": 0, "character": 0 },
                                      "end": { "line": 0, "character": 3 } },
            "targetUri": "file:///b.rs",
            "targetRange": { "start": { "line": 10, "character": 0 },
                             "end": { "line": 20, "character": 1 } },
            "targetSelectionRange": { "start": { "line": 10, "character": 4 },
                                      "end": { "line": 10, "character": 9 } }
        }))
        .unwrap();
        assert_eq!(link.target_selection_range.start.character, 4, "the symbol, not the block");

        let edit: TextEdit = serde_json::from_value(json!({
            "range": { "start": { "line": 0, "character": 0 },
                       "end": { "line": 1, "character": 0 } },
            "newText": "fn main() {\n"
        }))
        .unwrap();
        assert_eq!(edit.new_text, "fn main() {\n");
    }

    #[test]
    fn progress_decodes_its_three_kinds() {
        let begin: WorkDone =
            serde_json::from_value(json!({ "kind": "begin", "title": "indexing" })).unwrap();
        assert!(matches!(begin, WorkDone::Begin { title, .. } if title == "indexing"));

        let report: WorkDone =
            serde_json::from_value(json!({ "kind": "report", "percentage": 40 })).unwrap();
        assert!(matches!(report, WorkDone::Report { percentage: Some(40), .. }));

        let end: WorkDone = serde_json::from_value(json!({ "kind": "end" })).unwrap();
        assert!(matches!(end, WorkDone::End { .. }));
    }
}
