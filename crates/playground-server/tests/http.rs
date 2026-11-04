//! Playground-server tests (issue 038 TDD): the routing table, request
//! bounds (413), pack dispatch, the deterministic JSON snapshot shape,
//! HTTP request parsing, and shutdown correctness over a real socket.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use playground_server::{Method, Registry, ReqError, Request, Server, parse_request, route};

fn registry() -> Registry {
    Registry::reference()
}

fn get(path: &str) -> Request {
    Request {
        method: Method::Get,
        path: path.to_owned(),
        body: Vec::new(),
    }
}

fn post(path: &str, body: &[u8]) -> Request {
    Request {
        method: Method::Post,
        path: path.to_owned(),
        body: body.to_vec(),
    }
}

// ---------------------------------------------------------------------------
// Routing table
// ---------------------------------------------------------------------------

#[test]
fn get_packs_lists_registered_packs_in_order() {
    let resp = route(&registry(), 1024, &get("/packs"));
    assert_eq!(resp.status, 200);
    assert_eq!(
        resp.body,
        "{\"packs\":[\"json\",\"toml\",\"python\",\"typescript\",\"rust\",\"c\",\"sql\"]}"
    );
}

#[test]
fn unknown_path_is_404() {
    let resp = route(&registry(), 1024, &get("/nope"));
    assert_eq!(resp.status, 404);
    assert_eq!(resp.body, "{\"error\":\"not found\"}");
}

#[test]
fn wrong_method_on_known_path_is_405() {
    let resp = route(&registry(), 1024, &get("/parse"));
    assert_eq!(resp.status, 405);
    assert_eq!(resp.body, "{\"error\":\"method not allowed\"}");
    let resp = route(&registry(), 1024, &post("/packs", b""));
    assert_eq!(resp.status, 405);
}

// ---------------------------------------------------------------------------
// Request bounds: 413
// ---------------------------------------------------------------------------

#[test]
fn oversized_parse_body_is_413() {
    let big = vec![b'a'; 2048];
    let req = Request {
        method: Method::Post,
        path: "/parse".to_owned(),
        body: big,
    };
    let resp = route(&registry(), 1024, &req);
    assert_eq!(resp.status, 413);
    assert_eq!(resp.body, "{\"error\":\"request body too large\"}");
}

#[test]
fn oversized_content_length_is_rejected_at_http_layer() {
    let raw = b"POST /parse HTTP/1.1\r\nContent-Length: 99999\r\n\r\n";
    let err = parse_request(raw, 1024).expect_err("too large");
    assert_eq!(err, ReqError::TooLarge);
}

// ---------------------------------------------------------------------------
// Pack dispatch + 400/404
// ---------------------------------------------------------------------------

#[test]
fn malformed_parse_body_is_400() {
    let resp = route(&registry(), 1024, &post("/parse", b"not json"));
    assert_eq!(resp.status, 400);
    assert_eq!(resp.body, "{\"error\":\"malformed request body\"}");
}

#[test]
fn missing_source_is_400() {
    let resp = route(&registry(), 1024, &post("/parse", br#"{"pack":"json"}"#));
    assert_eq!(resp.status, 400);
}

#[test]
fn unknown_pack_is_404() {
    let resp = route(
        &registry(),
        1024,
        &post("/parse", br#"{"pack":"cobol","source":"1"}"#),
    );
    assert_eq!(resp.status, 404);
    assert_eq!(resp.body, "{\"error\":\"unknown pack: cobol\"}");
}

// ---------------------------------------------------------------------------
// JSON snapshot shape (deterministic serialization)
// ---------------------------------------------------------------------------

#[test]
fn parse_json_snapshot_shape() {
    let resp = route(
        &registry(),
        1024,
        &post("/parse", br#"{"pack":"json","source":"[1, 2]"}"#),
    );
    assert_eq!(resp.status, 200);
    pretty_assertions::assert_eq!(
        resp.body,
        "{\n  \"pack\": \"json\",\n  \"ok\": true,\n  \"tokens\": [\n    {\"kind\": 10, \"start\": 0, \"end\": 1},\n    {\"kind\": 4, \"start\": 1, \"end\": 2},\n    {\"kind\": 13, \"start\": 2, \"end\": 3},\n    {\"kind\": 14, \"start\": 3, \"end\": 4},\n    {\"kind\": 4, \"start\": 4, \"end\": 5},\n    {\"kind\": 11, \"start\": 5, \"end\": 6},\n    {\"kind\": 65535, \"start\": 6, \"end\": 6}\n  ],\n  \"tree\": {\"node\": {\"kind\": 65532, \"error\": false, \"children\": [\n    {\"node\": {\"kind\": 1, \"error\": false, \"children\": [\n      {\"token\": {\"kind\": 10, \"range\": [0, 1]}},\n      {\"token\": {\"kind\": 4, \"range\": [1, 2]}},\n      {\"token\": {\"kind\": 13, \"range\": [2, 3]}},\n      {\"token\": {\"kind\": 14, \"range\": [3, 4]}},\n      {\"token\": {\"kind\": 4, \"range\": [4, 5]}},\n      {\"token\": {\"kind\": 11, \"range\": [5, 6]}}\n    ]}},\n    {\"token\": {\"kind\": 65535, \"range\": [6, 6]}}\n  ]}}\n}"
    );
}

#[test]
fn parse_error_reported_not_failed() {
    // Malformed input never fails the request: recovery keeps it lossless,
    // the response just carries ok=false.
    let resp = route(
        &registry(),
        1024,
        &post("/parse", br#"{"pack":"json","source":"{]"}"#),
    );
    assert_eq!(resp.status, 200);
    assert!(resp.body.contains("\"ok\": false"), "{}", resp.body);
}

#[test]
fn parse_is_deterministic() {
    let body = br#"{"pack":"toml","source":"a = 1\n"}"#;
    let a = route(&registry(), 1024, &post("/parse", body)).body;
    let b = route(&registry(), 1024, &post("/parse", body)).body;
    assert_eq!(a, b);
}

// ---------------------------------------------------------------------------
// HTTP layer
// ---------------------------------------------------------------------------

#[test]
fn parses_a_full_request() {
    let raw =
        b"POST /parse HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 4\r\n\r\nbody";
    let req = parse_request(raw, 1024).expect("parses");
    assert!(matches!(req.method, Method::Post));
    assert_eq!(req.path, "/parse");
    assert_eq!(req.body, b"body");
}

#[test]
fn truncated_request_is_bad() {
    let raw = b"GET /packs HTTP/1.1\r\n";
    let err = parse_request(raw, 1024).expect_err("truncated");
    assert_eq!(err, ReqError::BadRequest);
}

#[test]
fn garbage_request_line_is_bad() {
    let raw = b"NONSENSE\r\n\r\n";
    let err = parse_request(raw, 1024).expect_err("garbage");
    assert_eq!(err, ReqError::BadRequest);
}

#[test]
fn response_has_headers_and_length() {
    use playground_server::{Response, serialize_response};
    let wire = serialize_response(&Response {
        status: 200,
        body: "{}".to_owned(),
    });
    let text = String::from_utf8(wire).expect("utf8");
    assert!(
        text.starts_with("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}"),
        "{text}"
    );
}

// ---------------------------------------------------------------------------
// End-to-end over a real socket + shutdown correctness
// ---------------------------------------------------------------------------

#[test]
fn server_serves_requests_and_shuts_down() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let shutdown = Arc::new(AtomicBool::new(false));
    let server = Server {
        registry: registry(),
        max_body: 1024,
        shutdown: Arc::clone(&shutdown),
        poll: Duration::from_millis(2),
    };
    let handle = std::thread::spawn(move || server.serve(&listener));

    // A real end-to-end request through the socket.
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    stream
        .write_all(b"GET /packs HTTP/1.1\r\nContent-Length: 0\r\n\r\n")
        .expect("write");
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).expect("read to close");
    let text = String::from_utf8(buf).expect("utf8");
    assert!(
        text.contains("{\"packs\":[\"json\""),
        "packs over the wire: {text}"
    );

    // Graceful shutdown: the flag ends the accept loop, serve returns.
    shutdown.store(true, Ordering::Relaxed);
    let result = handle.join().expect("join");
    assert!(result.is_ok(), "serve returns Ok on shutdown");
}

#[test]
fn server_rejects_oversized_bodies_over_the_wire() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let shutdown = Arc::new(AtomicBool::new(false));
    let server = Server {
        registry: registry(),
        max_body: 16,
        shutdown: Arc::clone(&shutdown),
        poll: Duration::from_millis(2),
    };
    let handle = std::thread::spawn(move || server.serve(&listener));

    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    let payload = vec![b'x'; 64];
    stream
        .write_all(b"POST /parse HTTP/1.1\r\nContent-Length: 64\r\n\r\n")
        .expect("write headers");
    stream.write_all(&payload).expect("write body");
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).expect("read to close");
    let text = String::from_utf8(buf).expect("utf8");
    assert!(text.starts_with("HTTP/1.1 413"), "{text}");

    shutdown.store(true, Ordering::Relaxed);
    assert!(handle.join().expect("join").is_ok());
}
