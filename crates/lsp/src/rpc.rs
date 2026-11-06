//! LSP base-protocol framing (issue 041): `Content-Length` headers,
//! bounded message sizes, and the JSON-RPC 2.0 message layer. Pure
//! byte-level policy — the transport only moves bytes (the
//! playground-server split, applied to stdio).

use std::io::{BufRead, Write};

use serde_json::Value;

/// Upper bound for one framed message (8 MiB — grammar files are small;
/// the bound exists so a broken peer cannot buffer us forever).
pub const MAX_MESSAGE: usize = 8 * 1024 * 1024;

/// JSON-RPC error codes used by the server.
pub const PARSE_ERROR: i64 = -32700;
/// Malformed JSON-RPC envelope.
pub const INVALID_REQUEST: i64 = -32600;
/// Unknown request method.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// Bad params for a known method.
pub const INVALID_PARAMS: i64 = -32602;
/// Unexpected server-side failure.
pub const INTERNAL_ERROR: i64 = -32603;
/// Request before `initialize`.
pub const SERVER_NOT_INITIALIZED: i64 = -32002;

/// Why a framed message could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// Header block or body did not parse (missing `Content-Length`,
    /// non-numeric length, truncated body).
    Malformed,
    /// Declared `Content-Length` beyond [`MAX_MESSAGE`].
    TooLarge,
    /// Clean EOF before any header bytes (the client hung up).
    Eof,
}

/// A JSON-RPC request id (number or string).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Id {
    /// Numeric id.
    Num(i64),
    /// String id.
    Str(String),
}

impl Id {
    /// Serialize the id as JSON.
    #[must_use]
    pub fn to_value(&self) -> Value {
        match self {
            Self::Num(n) => Value::from(*n),
            Self::Str(s) => Value::from(s.as_str()),
        }
    }
}

/// One decoded incoming message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    /// A request expects a response keyed by `id`.
    Request {
        /// The request id.
        id: Id,
        /// The method name.
        method: String,
        /// The params object, when present.
        params: Option<Value>,
    },
    /// A notification never gets a response.
    Notification {
        /// The method name.
        method: String,
        /// The params object, when present.
        params: Option<Value>,
    },
}

/// Why an incoming body failed to decode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcError {
    /// Not valid JSON.
    Parse,
    /// Valid JSON, not a valid JSON-RPC 2.0 message.
    Shape,
}

/// Read one framed message body from `reader` (headers + body).
///
/// `max_message` bounds the declared `Content-Length`.
///
/// # Errors
/// [`FrameError::Eof`] on a clean hangup at a message boundary,
/// [`FrameError::Malformed`] on truncated/malformed input,
/// [`FrameError::TooLarge`] beyond `max_message`.
pub fn read_frame(reader: &mut impl BufRead, max_message: usize) -> Result<Vec<u8>, FrameError> {
    let mut content_length: Option<usize> = None;
    let mut line = Vec::new();
    let mut first = true;
    loop {
        line.clear();
        let read = reader
            .read_until(b'\n', &mut line)
            .map_err(|_| FrameError::Malformed)?;
        if read == 0 {
            // Clean hangup at a message boundary vs mid-header garbage.
            return if first && line.is_empty() {
                Err(FrameError::Eof)
            } else {
                Err(FrameError::Malformed)
            };
        }
        first = false;
        let trimmed = trim_eol(&line);
        if trimmed.is_empty() {
            break; // end of the header block
        }
        let header = std::str::from_utf8(trimmed).map_err(|_| FrameError::Malformed)?;
        if let Some((name, value)) = header.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = Some(value.trim().parse().map_err(|_| FrameError::Malformed)?);
            }
        }
    }
    let length = content_length.ok_or(FrameError::Malformed)?;
    if length > max_message {
        return Err(FrameError::TooLarge);
    }
    let mut body = vec![0u8; length];
    reader
        .read_exact(&mut body)
        .map_err(|_| FrameError::Malformed)?;
    Ok(body)
}

/// Serialize one message with LSP framing onto `writer`.
///
/// # Errors
/// Propagates `io::Write` failures.
pub fn write_frame(writer: &mut impl Write, body: &[u8]) -> std::io::Result<()> {
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(body)?;
    writer.flush()
}

fn trim_eol(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    if end > 0 && line[end - 1] == b'\n' {
        end -= 1;
        if end > 0 && line[end - 1] == b'\r' {
            end -= 1;
        }
    }
    &line[..end]
}

/// Decode one incoming message body.
///
/// # Errors
/// [`RpcError::Parse`] for invalid JSON, [`RpcError::Shape`] for a
/// valid value that is not a JSON-RPC request/notification.
pub fn parse_incoming(body: &[u8]) -> Result<Incoming, RpcError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| RpcError::Parse)?;
    let obj = value.as_object().ok_or(RpcError::Shape)?;
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(RpcError::Shape);
    }
    let method = obj
        .get("method")
        .and_then(Value::as_str)
        .ok_or(RpcError::Shape)?
        .to_owned();
    let params = obj.get("params").cloned().filter(Value::is_object);
    match obj.get("id") {
        None => Ok(Incoming::Notification { method, params }),
        Some(raw) => {
            let id = parse_id(raw).ok_or(RpcError::Shape)?;
            Ok(Incoming::Request { id, method, params })
        }
    }
}

fn parse_id(raw: &Value) -> Option<Id> {
    match raw {
        Value::Number(n) => n.as_i64().map(Id::Num),
        Value::String(s) => Some(Id::Str(s.clone())),
        _ => None,
    }
}

/// A success response envelope.
#[must_use]
pub fn response_ok(id: &Id, result: Value) -> Value {
    let mut object = serde_json::Map::new();
    object.insert("jsonrpc".into(), Value::from("2.0"));
    object.insert("id".into(), id.to_value());
    object.insert("result".into(), result);
    Value::Object(object)
}

/// An error response envelope.
#[must_use]
pub fn response_err(id: &Id, code: i64, message: &str) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id.to_value(),
        "error": { "code": code, "message": message }
    })
}

/// A notification envelope.
#[must_use]
pub fn notification(method: &str, params: Value) -> Value {
    let mut object = serde_json::Map::new();
    object.insert("jsonrpc".into(), Value::from("2.0"));
    object.insert("method".into(), Value::from(method));
    object.insert("params".into(), params);
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    // -----------------------------------------------------------------
    // Framing
    // -----------------------------------------------------------------

    #[test]
    fn frame_round_trips_through_read_and_write() {
        let mut buf = Vec::new();
        write_frame(&mut buf, br#"{"jsonrpc":"2.0"}"#).expect("write");
        let mut reader = BufReader::new(&buf[..]);
        let body = read_frame(&mut reader, MAX_MESSAGE).expect("read");
        assert_eq!(body, br#"{"jsonrpc":"2.0"}"#);
    }

    #[test]
    fn frame_headers_are_case_insensitive_and_skip_extras() {
        let raw = b"content-LENGTH: 2\r\nX-Extra: ignored\r\n\r\nhi";
        let mut reader = BufReader::new(&raw[..]);
        let body = read_frame(&mut reader, MAX_MESSAGE).expect("read");
        assert_eq!(body, b"hi");
    }

    #[test]
    fn frame_without_content_length_is_malformed() {
        let raw = b"X-Other: 1\r\n\r\nbody";
        let mut reader = BufReader::new(&raw[..]);
        assert_eq!(
            read_frame(&mut reader, MAX_MESSAGE),
            Err(FrameError::Malformed)
        );
    }

    #[test]
    fn frame_with_short_body_is_malformed() {
        let raw = b"Content-Length: 10\r\n\r\nshort";
        let mut reader = BufReader::new(&raw[..]);
        assert_eq!(
            read_frame(&mut reader, MAX_MESSAGE),
            Err(FrameError::Malformed)
        );
    }

    #[test]
    fn frame_over_the_bound_is_too_large() {
        let raw = format!("Content-Length: {}\r\n\r\n", MAX_MESSAGE + 1);
        let mut reader = BufReader::new(raw.as_bytes());
        assert_eq!(
            read_frame(&mut reader, MAX_MESSAGE),
            Err(FrameError::TooLarge)
        );
        // A caller-provided smaller bound also applies.
        let raw = b"Content-Length: 3\r\n\r\nabc";
        let mut reader = BufReader::new(&raw[..]);
        assert_eq!(read_frame(&mut reader, 2), Err(FrameError::TooLarge));
    }

    #[test]
    fn frame_at_eof_is_malformed_not_panic() {
        let mut reader = BufReader::new(&b""[..]);
        assert_eq!(read_frame(&mut reader, MAX_MESSAGE), Err(FrameError::Eof));
    }

    #[test]
    fn frame_eof_after_partial_header_is_malformed() {
        let mut reader = BufReader::new(&b"Content-Length: 5\r\n"[..]);
        assert_eq!(
            read_frame(&mut reader, MAX_MESSAGE),
            Err(FrameError::Malformed)
        );
    }

    #[test]
    fn two_frames_read_in_sequence() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"one").expect("write");
        write_frame(&mut buf, b"two-two").expect("write");
        let mut reader = BufReader::new(&buf[..]);
        assert_eq!(read_frame(&mut reader, MAX_MESSAGE).expect("first"), b"one");
        assert_eq!(
            read_frame(&mut reader, MAX_MESSAGE).expect("second"),
            b"two-two"
        );
    }

    // -----------------------------------------------------------------
    // JSON-RPC layer
    // -----------------------------------------------------------------

    #[test]
    fn request_with_numeric_id_parses() {
        let incoming =
            parse_incoming(br#"{"jsonrpc":"2.0","id":7,"method":"initialize"}"#).expect("parses");
        assert_eq!(
            incoming,
            Incoming::Request {
                id: Id::Num(7),
                method: "initialize".to_owned(),
                params: None
            }
        );
    }

    #[test]
    fn request_with_string_id_and_params_parses() {
        let body = br#"{"jsonrpc":"2.0","id":"a-1","method":"m","params":{"x":1}}"#;
        let incoming = parse_incoming(body).expect("parses");
        assert_eq!(
            incoming,
            Incoming::Request {
                id: Id::Str("a-1".to_owned()),
                method: "m".to_owned(),
                params: Some(serde_json::json!({"x": 1}))
            }
        );
    }

    #[test]
    fn notification_without_id_parses() {
        let body = br#"{"jsonrpc":"2.0","method":"exit"}"#;
        let incoming = parse_incoming(body).expect("parses");
        assert_eq!(
            incoming,
            Incoming::Notification {
                method: "exit".to_owned(),
                params: None
            }
        );
    }

    #[test]
    fn array_params_are_dropped_not_rejected() {
        let body = br#"{"jsonrpc":"2.0","id":1,"method":"m","params":[1,2]}"#;
        let incoming = parse_incoming(body).expect("parses");
        assert!(matches!(incoming, Incoming::Request { params: None, .. }));
    }

    #[test]
    fn bad_json_is_a_parse_error() {
        assert_eq!(parse_incoming(b"not json"), Err(RpcError::Parse));
    }

    #[test]
    fn wrong_envelopes_are_shape_errors() {
        assert_eq!(parse_incoming(b"[]"), Err(RpcError::Shape));
        assert_eq!(
            parse_incoming(br#"{"id":1,"method":"m"}"#),
            Err(RpcError::Shape)
        );
        assert_eq!(
            parse_incoming(br#"{"jsonrpc":"1.0","id":1,"method":"m"}"#),
            Err(RpcError::Shape)
        );
        assert_eq!(
            parse_incoming(br#"{"jsonrpc":"2.0","id":1,"method":3}"#),
            Err(RpcError::Shape)
        );
        assert_eq!(
            parse_incoming(br#"{"jsonrpc":"2.0","id":{},"method":"m"}"#),
            Err(RpcError::Shape)
        );
    }

    #[test]
    fn envelopes_serialize_with_expected_keys() {
        let ok = response_ok(&Id::Num(1), serde_json::json!({"caps": true}));
        assert_eq!(ok["id"], serde_json::json!(1));
        assert_eq!(ok["result"]["caps"], serde_json::json!(true));
        assert!(ok.get("error").is_none());

        let err = response_err(&Id::Str("x".into()), METHOD_NOT_FOUND, "nope");
        assert_eq!(err["error"]["code"], serde_json::json!(METHOD_NOT_FOUND));
        assert_eq!(err["id"], serde_json::json!("x"));

        let note = notification(
            "textDocument/publishDiagnostics",
            serde_json::json!({"uri": "u"}),
        );
        assert!(note.get("id").is_none());
        assert_eq!(
            note["method"],
            serde_json::json!("textDocument/publishDiagnostics")
        );
    }
}
