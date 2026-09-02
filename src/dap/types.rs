//! The protocol structs bi actually uses from a DAP adapter's replies and
//! events — a fraction of a spec with dozens of request/response pairs.
//! Owned rather than imported, exactly as `lsp::types` does, so each shape
//! is precisely what bi's client and registry read and nothing more.
//!
//! Everything here is defensive: adapters disagree on which optional fields
//! they bother sending, so every field bi does not require for correctness
//! is `#[serde(default)]` — an absent capability or line number is an
//! answer, never a parse failure. See `docs/specs/debug.md` for the
//! lifecycle these structs are threaded through.

use serde::{Deserialize, Serialize};

/// The body of `initialize`'s response — what the adapter can do. bi keeps
/// only the three capabilities that change client behaviour; the rest of
/// the (much larger) capabilities object is simply not modeled.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct InitializeResponseBody {
    #[serde(default)]
    pub capabilities: Capabilities,
}

/// Adapter capabilities bi gates features on. All default to `false` —
/// an adapter that stays silent about a feature is assumed not to have it,
/// same as `lsp::types::Capabilities`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    /// Whether `configurationDone` must be sent to let the program start —
    /// gates step 4 of the lifecycle (push breakpoints, then this, then run).
    #[serde(default)]
    pub supports_configuration_done_request: bool,
    /// Whether a breakpoint's `condition` field is honoured. Without this,
    /// bi still lets a condition be typed but the adapter ignores it.
    #[serde(default)]
    pub supports_conditional_breakpoints: bool,
    /// Whether `terminate` (graceful "ask the debuggee to exit") exists as
    /// distinct from `disconnect` (just detach). Without it, ending a
    /// session always falls back to `disconnect`.
    #[serde(default)]
    pub supports_terminate_request: bool,
}

/// The `stopped` event — the cue, per the lifecycle doc, to walk
/// `threads` → `stackTrace` → `scopes` → `variables`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoppedEvent {
    /// "breakpoint" / "step" / "pause" / "exception" / adapter-specific text.
    /// bi does not enumerate these — they drive a status line message, not
    /// branching logic.
    pub reason: String,
    /// The thread that actually stopped. Absent is rare but legal (some
    /// adapters send `allThreadsStopped` instead of naming one), so this
    /// stays optional rather than defaulting to a thread that doesn't exist.
    #[serde(default)]
    pub thread_id: Option<i64>,
}

/// `output` event — the debuggee's stdout/stderr, and the adapter's own
/// diagnostic chatter, arrive this way rather than over a pipe bi reads
/// itself (see "launch: adapter owns the child's streams" in the spec).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct OutputEvent {
    /// "console" / "stdout" / "stderr" / "telemetry" / ... . Absent means
    /// "console", the spec's own default; bi keeps the absence rather than
    /// filling it in so a caller can tell "unlabeled" from "labeled console".
    #[serde(default)]
    pub category: Option<String>,
    pub output: String,
}

/// One row of a `threads` response.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Thread {
    pub id: i64,
    pub name: String,
}

/// One row of a `stackTrace` response — bi keeps only what draws the frame
/// list and jumps the cursor: name, source file, and 1-based line/column.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct StackFrame {
    pub id: i64,
    pub name: String,
    /// Absent for frames with no source (e.g. inside a library with no
    /// debug info) — bi shows the frame but cannot jump to it.
    #[serde(default)]
    pub source: Option<Source>,
    pub line: i64,
    pub column: i64,
}

/// A DAP `Source` reduced to the one field bi can act on. The spec's
/// `Source` also carries `sourceReference` for adapter-held pseudo-files
/// (disassembly, decompiled output); bi does not support those yet, so a
/// frame with no `path` simply cannot be jumped to.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Source {
    #[serde(default)]
    pub path: Option<String>,
}

/// One row of a `scopes` response — "Locals", "Arguments", etc. Expanding
/// it means requesting `variables_reference` (0 means empty, never sent).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scope {
    pub name: String,
    pub variables_reference: i64,
    /// Costly to evaluate (e.g. it runs code) — bi defers fetching these
    /// until asked rather than eagerly expanding every scope on a stop.
    #[serde(default)]
    pub expensive: bool,
}

/// One row of a `variables` response. `variables_reference` nonzero means
/// this value has children (a struct's fields, a `Vec`'s elements) fetched
/// by the same `variables` request against that reference — the lazy tree
/// the lifecycle doc describes.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Variable {
    pub name: String,
    /// Already formatted by the adapter — bi displays it verbatim rather
    /// than re-deriving a representation from the value's type.
    pub value: String,
    pub variables_reference: i64,
    /// Renamed off `type`, a reserved word. Adapter-supplied and often
    /// absent for adapters that don't bother (e.g. dynamically typed
    /// languages), so this stays optional rather than "".
    #[serde(default, rename = "type")]
    pub ty: Option<String>,
}

/// One breakpoint as the adapter reports it back, in a `setBreakpoints`
/// response or a `breakpoint` event. `line` may differ from what bi asked
/// for — "moved" per the lifecycle doc, when the requested line has no
/// code — so the gutter must reflect the answer, not the request.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Breakpoint {
    pub verified: bool,
    #[serde(default)]
    pub line: Option<i64>,
    /// Set when `verified` is false and the adapter explains why (e.g. "No
    /// symbols loaded"), or on any other rejection. Shown, not parsed.
    #[serde(default)]
    pub message: Option<String>,
}

/// One breakpoint as bi *sends* it in `setBreakpoints`'s `breakpoints`
/// array — editor-owned, per the lifecycle doc, so this is the minimal
/// shape a line number needs to become a request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceBreakpoint {
    pub line: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_response_reads_capabilities_and_defaults_missing_ones() {
        let v = serde_json::json!({
            "capabilities": { "supportsConfigurationDoneRequest": true }
        });
        let body: InitializeResponseBody = serde_json::from_value(v).unwrap();
        assert!(body.capabilities.supports_configuration_done_request);
        // Absent capability defaults to false, never an error.
        assert!(!body.capabilities.supports_conditional_breakpoints);
    }

    #[test]
    fn a_stopped_event_keeps_reason_and_thread() {
        let v = serde_json::json!({ "reason": "breakpoint", "threadId": 1 });
        let ev: StoppedEvent = serde_json::from_value(v).unwrap();
        assert_eq!(ev.reason, "breakpoint");
        assert_eq!(ev.thread_id, Some(1));
    }

    #[test]
    fn a_variable_with_children_carries_a_nonzero_reference() {
        let v = serde_json::json!({
            "name": "v", "value": "Vec", "variablesReference": 7, "type": "Vec<u8>"
        });
        let var: Variable = serde_json::from_value(v).unwrap();
        assert_eq!(var.variables_reference, 7);
        assert_eq!(var.ty.as_deref(), Some("Vec<u8>"));
    }

    #[test]
    fn a_moved_breakpoint_reports_its_new_line() {
        let v = serde_json::json!({ "verified": true, "line": 42 });
        let bp: Breakpoint = serde_json::from_value(v).unwrap();
        assert!(bp.verified);
        assert_eq!(bp.line, Some(42));
    }
}
