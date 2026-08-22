//! JSON-RPC 2.0 envelopes and the `Content-Length` framing LSP wraps them in.
//!
//! Pure: bytes and `serde_json::Value`s in, the same out. The process and its
//! pipes live in `transport.rs`; nothing here blocks on anything but the
//! reader it is handed.

use std::io::{self, BufRead};

use serde_json::{Value, json};

use super::Inbound;

/// The error half of a response, kept as the wire gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseError {
    pub code: i64,
    pub message: String,
}

/// JSON-RPC's "no such method". The honest answer to a server request bi
/// does not implement — and the answer the protocol is designed around, so a
/// server that gets it carries on.
pub const METHOD_NOT_FOUND: i64 = -32601;

pub fn request(id: i64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

pub fn notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

/// A response to a server's request. The id is echoed verbatim — the server
/// picked its shape, and number-vs-string is its business.
pub fn response_ok(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub fn response_err(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// One message as it goes down the pipe: headers, blank line, body.
pub fn encode(msg: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(msg).expect("a Value always serializes");
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(&body);
    out
}

/// Reads one frame's body. `Ok(None)` is EOF at a frame boundary — the
/// server closed cleanly. EOF mid-frame or a malformed header is an error,
/// because half a message means the stream can never be trusted again.
pub fn read_frame(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut length: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return match length {
                // EOF between frames is how servers end; EOF after a header
                // has started is a truncated message.
                None => Ok(None),
                Some(_) => Err(io::Error::new(io::ErrorKind::UnexpectedEof, "EOF mid-frame")),
            };
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break; // the blank line before the body
        }
        // `Content-Type` is the only other header the spec names; anything
        // unrecognized is skipped rather than fatal, as headers are meant to
        // be read.
        if let Some(value) = header_value(trimmed, "Content-Length") {
            length = Some(value.trim().parse().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, format!("bad Content-Length: {value}"))
            })?);
        }
    }
    let Some(length) = length else {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame without Content-Length"));
    };
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    Ok(Some(body))
}

/// The value of `Name: value`, matched case-insensitively — the header names
/// are defined ASCII, and case-insensitive is what HTTP-style headers are.
fn header_value<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let (key, value) = line.split_once(':')?;
    key.trim().eq_ignore_ascii_case(name).then_some(value)
}

/// Classifies one decoded body. A `method` makes it a request or
/// notification; an `id` without one makes it a response.
pub fn decode(body: &[u8]) -> Result<Inbound, String> {
    let mut msg: Value = serde_json::from_slice(body).map_err(|e| e.to_string())?;
    let obj = msg.as_object_mut().ok_or("a message that is not an object")?;

    if let Some(method) = obj.get("method").and_then(Value::as_str).map(String::from) {
        let params = obj.remove("params").unwrap_or(Value::Null);
        return Ok(match obj.remove("id") {
            Some(id) => Inbound::Request { id, method, params },
            None => Inbound::Notification { method, params },
        });
    }

    let id = obj
        .get("id")
        .and_then(Value::as_i64)
        .ok_or("a response without an integer id — bi never sent that request")?;
    let result = match obj.remove("error") {
        Some(error) => Err(ResponseError {
            code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("no message")
                .to_string(),
        }),
        None => Ok(obj.remove("result").unwrap_or(Value::Null)),
    };
    Ok(Inbound::Response { id, result })
}

#[cfg(test)]
mod tests {
    use std::io::BufReader;

    use super::*;

    #[test]
    fn a_frame_round_trips() {
        let msg = request(7, "initialize", json!({ "a": 1 }));
        let bytes = encode(&msg);
        let mut reader = BufReader::new(bytes.as_slice());
        let body = read_frame(&mut reader).unwrap().expect("one frame");
        match decode(&body).unwrap() {
            Inbound::Response { .. } | Inbound::Notification { .. } => panic!("it had id+method"),
            Inbound::Request { id, method, params } => {
                assert_eq!(id, json!(7));
                assert_eq!(method, "initialize");
                assert_eq!(params, json!({ "a": 1 }));
            }
            Inbound::Eof => unreachable!(),
        }
    }

    #[test]
    fn two_frames_read_in_order_and_then_a_clean_eof() {
        let mut bytes = encode(&notification("a", Value::Null));
        bytes.extend(encode(&notification("b", Value::Null)));
        let mut reader = BufReader::new(bytes.as_slice());

        for expected in ["a", "b"] {
            let body = read_frame(&mut reader).unwrap().expect("a frame");
            match decode(&body).unwrap() {
                Inbound::Notification { method, .. } => assert_eq!(method, expected),
                other => panic!("{other:?}"),
            }
        }
        assert!(read_frame(&mut reader).unwrap().is_none(), "clean EOF");
    }

    #[test]
    fn eof_mid_frame_is_an_error_not_a_clean_end() {
        let bytes = b"Content-Length: 100\r\n\r\n{\"partial\":";
        let mut reader = BufReader::new(bytes.as_slice());
        assert!(read_frame(&mut reader).is_err());

        let headers_only = b"Content-Length: 5\r\n";
        let mut reader = BufReader::new(headers_only.as_slice());
        assert!(read_frame(&mut reader).is_err(), "EOF after a header started");
    }

    #[test]
    fn unknown_headers_are_skipped_and_names_are_case_insensitive() {
        let body = br#"{"jsonrpc":"2.0","method":"m"}"#;
        let mut bytes =
            format!("Content-Type: application/json\r\ncontent-length: {}\r\n\r\n", body.len())
                .into_bytes();
        bytes.extend_from_slice(body);
        let mut reader = BufReader::new(bytes.as_slice());
        assert!(read_frame(&mut reader).unwrap().is_some());
    }

    #[test]
    fn a_frame_without_a_length_is_refused() {
        let bytes = b"Content-Type: application/json\r\n\r\n{}";
        let mut reader = BufReader::new(bytes.as_slice());
        assert!(read_frame(&mut reader).is_err());
    }

    #[test]
    fn responses_split_into_ok_and_error() {
        let ok = br#"{"jsonrpc":"2.0","id":3,"result":{"x":1}}"#;
        match decode(ok).unwrap() {
            Inbound::Response { id: 3, result: Ok(v) } => assert_eq!(v, json!({"x":1})),
            other => panic!("{other:?}"),
        }

        let err = br#"{"jsonrpc":"2.0","id":4,"error":{"code":-32601,"message":"nope"}}"#;
        match decode(err).unwrap() {
            Inbound::Response { id: 4, result: Err(e) } => {
                assert_eq!(e, ResponseError { code: METHOD_NOT_FOUND, message: "nope".into() });
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_null_result_is_still_a_response() {
        // rust-analyzer answers `shutdown` with `"result": null` — a response
        // with neither error nor a value worth keeping is still an answer.
        let body = br#"{"jsonrpc":"2.0","id":9,"result":null}"#;
        match decode(body).unwrap() {
            Inbound::Response { id: 9, result: Ok(Value::Null) } => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_server_request_keeps_its_id_verbatim() {
        // The id is the server's to shape — a string id must echo as one.
        let body =
            br#"{"jsonrpc":"2.0","id":"tok-1","method":"workspace/configuration","params":{}}"#;
        match decode(body).unwrap() {
            Inbound::Request { id, .. } => assert_eq!(id, json!("tok-1")),
            other => panic!("{other:?}"),
        }
        let echoed = response_ok(&json!("tok-1"), Value::Null);
        assert_eq!(echoed["id"], json!("tok-1"));
    }

    #[test]
    fn garbage_is_an_error_with_a_reason() {
        assert!(decode(b"not json").is_err());
        assert!(decode(br#""a string""#).is_err());
        // A response with a string id answers nothing bi sent.
        assert!(decode(br#"{"jsonrpc":"2.0","id":"s","result":1}"#).is_err());
    }
}
