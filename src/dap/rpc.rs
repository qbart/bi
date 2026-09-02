//! DAP's request/response/event envelope, over the same `Content-Length`
//! framing LSP uses — the two protocols share a transport, and only the
//! JSON shape inside each frame differs. Framing is reused verbatim from
//! `lsp::rpc`; only the envelope builders and `decode` are DAP-specific.
//!
//! Pure, like `lsp::rpc`: bytes and `serde_json::Value`s in, the same out.

use serde_json::{Value, json};

pub use crate::lsp::rpc::{encode, read_frame};

use super::Inbound;

/// A request bi sends to the adapter. `seq` is bi's own monotonic counter —
/// DAP, unlike LSP, numbers every message (requests, responses, events)
/// from a single sequence, so the caller supplies it rather than this
/// function inventing one.
pub fn request(seq: i64, command: &str, arguments: Value) -> Value {
    json!({ "seq": seq, "type": "request", "command": command, "arguments": arguments })
}

/// bi's answer to a *reverse* request — one the adapter sent it (e.g.
/// `runInTerminal`). `request_seq` echoes the adapter's `seq` so it can
/// match the answer back; `seq` is bi's own next sequence number, same as
/// any other outbound message.
pub fn response(request_seq: i64, seq: i64, command: &str, success: bool, body: Value) -> Value {
    json!({
        "seq": seq,
        "type": "response",
        "request_seq": request_seq,
        "success": success,
        "command": command,
        "body": body,
    })
}

/// Classifies one decoded body by DAP's `type` field — `"response"`,
/// `"event"`, or `"request"` (a reverse request; DAP overloads the same
/// word for both directions). Anything else is a protocol bi does not
/// speak. Defensive throughout: adapters vary in which optional fields
/// they bother sending, so `body`/`message` fall back rather than fail
/// to parse.
pub fn decode(body: &[u8]) -> Result<Inbound, String> {
    let mut msg: Value = serde_json::from_slice(body).map_err(|e| e.to_string())?;
    let obj = msg.as_object_mut().ok_or("a message that is not an object")?;

    let ty = obj.get("type").and_then(Value::as_str).ok_or("a message without a type")?;
    match ty {
        "response" => {
            let request_seq = obj
                .get("request_seq")
                .and_then(Value::as_i64)
                .ok_or("a response without an integer request_seq")?;
            let success = obj.get("success").and_then(Value::as_bool).unwrap_or(false);
            let command = obj
                .get("command")
                .and_then(Value::as_str)
                .ok_or("a response without a command")?
                .to_string();
            let body = obj.remove("body").unwrap_or(Value::Null);
            let message = obj.remove("message").and_then(|v| v.as_str().map(String::from));
            Ok(Inbound::Response { request_seq, success, command, body, message })
        }
        "event" => {
            let event = obj
                .get("event")
                .and_then(Value::as_str)
                .ok_or("an event without a name")?
                .to_string();
            let body = obj.remove("body").unwrap_or(Value::Null);
            Ok(Inbound::Event { event, body })
        }
        "request" => {
            let seq = obj
                .get("seq")
                .and_then(Value::as_i64)
                .ok_or("a reverse request without an integer seq")?;
            let command = obj
                .get("command")
                .and_then(Value::as_str)
                .ok_or("a reverse request without a command")?
                .to_string();
            let arguments = obj.remove("arguments").unwrap_or(Value::Null);
            Ok(Inbound::ReverseRequest { seq, command, arguments })
        }
        other => Err(format!("unknown message type: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_request_carries_seq_type_and_command() {
        let msg = request(3, "setBreakpoints", json!({"a":1}));
        assert_eq!(msg["seq"], 3);
        assert_eq!(msg["type"], "request");
        assert_eq!(msg["command"], "setBreakpoints");
        assert_eq!(msg["arguments"], json!({"a":1}));
    }

    #[test]
    fn a_response_decodes_with_its_request_seq_and_success() {
        let body = br#"{"seq":5,"type":"response","request_seq":3,"success":true,"command":"threads","body":{"threads":[]}}"#;
        match decode(body).unwrap() {
            Inbound::Response { request_seq: 3, success: true, command, body, .. } => {
                assert_eq!(command, "threads");
                assert_eq!(body, json!({"threads":[]}));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_failed_response_keeps_its_message() {
        let body = br#"{"seq":6,"type":"response","request_seq":4,"success":false,"command":"evaluate","message":"nope"}"#;
        match decode(body).unwrap() {
            Inbound::Response { success: false, message: Some(m), .. } => assert_eq!(m, "nope"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_event_decodes_by_its_name() {
        let body = br#"{"seq":1,"type":"event","event":"stopped","body":{"reason":"breakpoint","threadId":1}}"#;
        match decode(body).unwrap() {
            Inbound::Event { event, body } => {
                assert_eq!(event, "stopped");
                assert_eq!(body["threadId"], 1);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_reverse_request_keeps_seq_and_command() {
        let body =
            br#"{"seq":9,"type":"request","command":"runInTerminal","arguments":{"args":[]}}"#;
        match decode(body).unwrap() {
            Inbound::ReverseRequest { seq: 9, command, .. } => assert_eq!(command, "runInTerminal"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn garbage_and_unknown_types_are_errors_with_a_reason() {
        assert!(decode(b"not json").is_err());
        assert!(decode(br#"{"seq":1,"type":"nonsense"}"#).is_err());
    }
}
