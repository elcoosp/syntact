//! End-to-end session tests (issue 041 TDD): a scripted client drives
//! `transport::serve` over in-memory buffers through the full LSP
//! lifecycle — initialize → didOpen → queries → didChange → rename →
//! shutdown → exit — asserting every response on the wire.

use std::io::BufWriter;

use lsp::index::LineIndex;
use lsp::rpc::{self, FrameError};
use lsp::transport;
use serde_json::Value;

const MINI: &str = "\
language \"mini\" version=\"1.0.0\" extension=\"mini\"

tokens {
    number kind=re \"[0-9]+\"
    plus kind=lit \"+\"
}

grammar {
    expr   kind=alt number sum node=\"true\"
    sum    kind=seq plus expr
    unused kind=seq number
}
";

/// A scripted in-memory client: `send` queues a frame, and after
/// `serve` runs, `replies` decodes the responses in order.
#[derive(Default)]
struct Client {
    input: Vec<u8>,
    replies: Vec<Value>,
}

impl Client {
    fn send(&mut self, body: &str) {
        rpc::write_frame(&mut self.input, body.as_bytes()).expect("frame");
    }

    fn request(&mut self, id: i64, method: &str, params: &str) {
        self.send(&format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{params}}}"#
        ));
    }

    fn notification(&mut self, method: &str, params: &str) {
        self.send(&format!(
            r#"{{"jsonrpc":"2.0","method":"{method}","params":{params}}}"#
        ));
    }

    /// Run the server over the queued input and collect the replies.
    fn run(&mut self) -> i32 {
        let mut output = Vec::new();
        let code = {
            let mut writer = BufWriter::new(&mut output);
            let mut reader = std::io::Cursor::new(self.input.clone());
            transport::serve(&mut reader, &mut writer, 1 << 20)
        };
        let mut cursor = &output[..];
        loop {
            let mut reader = std::io::Cursor::new(cursor);
            match rpc::read_frame(&mut reader, 1 << 20) {
                Ok(body) => {
                    self.replies
                        .push(serde_json::from_slice(&body).expect("json response"));
                    cursor = &cursor[usize::try_from(reader.position()).expect("position")..];
                }
                Err(FrameError::Eof) => break,
                Err(other) => panic!("framing error mid-reply-stream: {other:?}"),
            }
        }
        code
    }

    fn pop(&mut self) -> Value {
        self.replies.remove(0)
    }
}

fn pos(text: &str, needle: &str) -> String {
    let index = LineIndex::new(text);
    let position = index.position(text, text.find(needle).expect("needle"));
    serde_json::to_string(&position).expect("position")
}

// One scripted narrative: splitting it would hide the protocol
// sequencing the test exists to pin.
#[test]
fn full_session_lifecycle() {
    let mut client = Client::default();

    // 1. initialize
    client.request(1, "initialize", "{}");
    // 2. initialized (notification, no reply)
    client.notification("initialized", "{}");
    // 3. didOpen (notification; pushes diagnostics)
    client.notification(
        "textDocument/didOpen",
        &format!(
            r#"{{"textDocument":{{"uri":"file:///m.gir","languageId":"gir","version":1,"text":{}}}}}"#,
            serde_json::to_string(MINI).expect("json")
        ),
    );
    // 4. hover on the `sum` declaration
    let hover_params = format!(
        r#"{{"textDocument":{{"uri":"file:///m.gir"}},"position":{}}}"#,
        pos(MINI, "sum    kind=seq")
    );
    client.request(2, "textDocument/hover", &hover_params);
    // 5. completion from inside the word `expr` (prefix-filtered)
    let complete_offset = MINI.find("plus expr").expect("body") + "plus ".len();
    let complete_params = format!(
        r#"{{"textDocument":{{"uri":"file:///m.gir"}},"position":{}}}"#,
        {
            let index = LineIndex::new(MINI);
            serde_json::to_string(&index.position(MINI, complete_offset)).expect("pos")
        }
    );
    client.request(3, "textDocument/completion", &complete_params);
    // 6. definition from the `expr` reference
    let ref_offset = MINI.find("plus expr").expect("body") + "plus ".len();
    let definition_params = format!(
        r#"{{"textDocument":{{"uri":"file:///m.gir"}},"position":{}}}"#,
        {
            let index = LineIndex::new(MINI);
            serde_json::to_string(&index.position(MINI, ref_offset)).expect("pos")
        }
    );
    client.request(4, "textDocument/definition", &definition_params);
    // 7. document symbols
    client.request(
        5,
        "textDocument/documentSymbol",
        r#"{"textDocument":{"uri":"file:///m.gir"}}"#,
    );
    // 8. incremental didChange breaking the grammar (`expr` → `expq`)
    let index = LineIndex::new(MINI);
    let start = MINI.find("plus expr").expect("body") + "plus ".len();
    let range = index.range(
        MINI,
        gir_syntax::ByteSpan {
            start,
            end: start + 4,
        },
    );
    client.notification(
        "textDocument/didChange",
        &format!(
            r#"{{"textDocument":{{"uri":"file:///m.gir"}},"contentChanges":[{{"range":{},"text":"expq"}}]}}"#,
            serde_json::to_string(&range).expect("range")
        ),
    );
    // 9. pull diagnostics reflect the broken state
    client.request(
        6,
        "textDocument/diagnostic",
        r#"{"textDocument":{"uri":"file:///m.gir"}}"#,
    );
    // 10. rename the intact `sum` rule → `total` (decl + its ref site;
    // the dangling `expq` reference is not a symbol and refuses rename)
    client.request(
        7,
        "textDocument/rename",
        &format!(
            r#"{{"textDocument":{{"uri":"file:///m.gir"}},"position":{},"newName":"total"}}"#,
            serde_json::to_string(
                &index.position(MINI, MINI.find("sum    kind=seq").expect("sum"))
            )
            .expect("pos")
        ),
    );
    // 11. shutdown + exit
    client.request(8, "shutdown", "null");
    client.notification("exit", "null");

    let code = client.run();
    assert_eq!(code, 0, "clean exit after shutdown");

    // 1. initialize capabilities.
    let response = client.pop();
    assert_eq!(response["id"], 1);
    assert_eq!(
        response["result"]["capabilities"]["positionEncoding"],
        "utf-16"
    );

    // 3. the didOpen diagnostics push (one hint: `unused` is dead).
    let push = client.pop();
    assert_eq!(push["method"], "textDocument/publishDiagnostics");
    assert_eq!(push["params"]["uri"], "file:///m.gir");
    let items = push["params"]["diagnostics"].as_array().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["severity"], 4);

    // 4. hover markdown.
    let response = client.pop();
    assert_eq!(response["id"], 2);
    assert!(
        response["result"]["contents"]["value"]
            .as_str()
            .expect("text")
            .contains("rule `sum`")
    );

    // 5. completion items (prefix `expr` filters to the rule alone).
    let response = client.pop();
    assert_eq!(response["id"], 3);
    let labels: Vec<&str> = response["result"]
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|item| item["label"].as_str())
        .collect();
    assert_eq!(labels, vec!["expr"]);

    // 6. definition location.
    let response = client.pop();
    assert_eq!(response["id"], 4);
    assert_eq!(response["result"]["uri"], "file:///m.gir");

    // 7. five symbols (two tokens, three rules).
    let response = client.pop();
    assert_eq!(response["id"], 5);
    let symbols = response["result"].as_array().expect("symbols");
    assert_eq!(symbols.len(), 5);

    // 8. the didChange push: `expq` is an unknown reference.
    let push = client.pop();
    assert_eq!(push["method"], "textDocument/publishDiagnostics");
    let items = push["params"]["diagnostics"].as_array().expect("items");
    assert!(
        items
            .iter()
            .any(|item| item["message"].as_str().is_some_and(|m| m.contains("expq"))),
        "{items:?}"
    );

    // 9. pull diagnostics agree.
    let response = client.pop();
    assert_eq!(response["id"], 6);
    assert_eq!(
        response["result"]["items"].as_array().expect("items").len(),
        items.len()
    );

    // 10. rename rewrote the `sum` declaration and its reference.
    let response = client.pop();
    assert_eq!(response["id"], 7);
    let edits = response["result"]["file:///m.gir"]
        .as_array()
        .expect("edits");
    assert_eq!(edits.len(), 2);
    assert!(edits.iter().all(|edit| edit["newText"] == "total"));

    // 11. shutdown result is null; `exit` pushes nothing.
    let response = client.pop();
    assert_eq!(response["id"], 8);
    assert_eq!(response["result"], Value::Null);
    assert_eq!(client.replies, [] as [serde_json::Value; 0]);
}

#[test]
fn session_refuses_queries_before_initialize_and_exits_unclean() {
    let mut client = Client::default();
    client.request(
        1,
        "textDocument/hover",
        r#"{"textDocument":{"uri":"file:///x.gir"},"position":{"line":0,"character":0}}"#,
    );
    client.notification("exit", "null");
    let code = client.run();
    assert_eq!(code, 1);
    let response = client.pop();
    assert_eq!(response["error"]["code"], -32002);
    assert_eq!(client.replies, [] as [serde_json::Value; 0]);
}

#[test]
fn multi_file_session_answers_per_document() {
    let other = "language \"oth\" version=\"1.0.0\"\ntokens {\n    x kind=lit \"x\"\n}\ngrammar {\n    s kind=seq x\n}\n";
    let mut client = Client::default();
    client.request(1, "initialize", "{}");
    client.notification("initialized", "{}");
    for (uri, text) in [("file:///a.gir", MINI), ("file:///b.gir", other)] {
        client.notification(
            "textDocument/didOpen",
            &format!(
                r#"{{"textDocument":{{"uri":"{uri}","text":{}}}}}"#,
                serde_json::to_string(text).expect("json")
            ),
        );
    }
    // symbols of both files in order.
    client.request(
        2,
        "textDocument/documentSymbol",
        r#"{"textDocument":{"uri":"file:///a.gir"}}"#,
    );
    client.request(
        3,
        "textDocument/documentSymbol",
        r#"{"textDocument":{"uri":"file:///b.gir"}}"#,
    );
    // hover the annotation `node` in a.
    client.request(
        4,
        "textDocument/hover",
        &format!(
            r#"{{"textDocument":{{"uri":"file:///a.gir"}},"position":{}}}"#,
            pos(MINI, "node=\"true\"")
        ),
    );
    client.request(9, "shutdown", "null");
    client.notification("exit", "null");

    assert_eq!(client.run(), 0);

    // initialize reply first, then the two didOpen pushes.
    assert_eq!(client.pop()["id"], 1);
    let push = client.pop();
    assert_eq!(push["params"]["uri"], "file:///a.gir");
    let push = client.pop();
    assert_eq!(push["params"]["uri"], "file:///b.gir");
    assert_eq!(
        push["params"]["diagnostics"]
            .as_array()
            .expect("items")
            .len(),
        0
    );

    let response = client.pop();
    assert_eq!(response["id"], 2);
    assert_eq!(response["result"].as_array().expect("symbols").len(), 5);
    let response = client.pop();
    assert_eq!(response["id"], 3);
    assert_eq!(response["result"].as_array().expect("symbols").len(), 2);

    let response = client.pop();
    assert_eq!(response["id"], 4);
    let value = response["result"]["contents"]["value"]
        .as_str()
        .expect("md");
    assert!(value.contains("`node`"), "{value}");
}

#[test]
fn positions_map_through_utf16_across_the_wire() {
    // A grammar with multibyte characters (inside KDL strings): the
    // position conversion must round-trip over the wire.
    let text =
        "language \"ünï\"\ntokens {\n    α kind=lit \"α😀\"\n}\ngrammar {\n    s kind=seq α\n}\n";
    let mut client = Client::default();
    client.request(1, "initialize", "{}");
    client.notification("initialized", "{}");
    client.notification(
        "textDocument/didOpen",
        &format!(
            r#"{{"textDocument":{{"uri":"file:///u.gir","text":{}}}}}"#,
            serde_json::to_string(text).expect("json")
        ),
    );
    // hover on the token name `α` (2 UTF-8 bytes, 1 UTF-16 unit).
    client.request(
        2,
        "textDocument/hover",
        &format!(
            r#"{{"textDocument":{{"uri":"file:///u.gir"}},"position":{}}}"#,
            pos(text, "α kind=lit")
        ),
    );
    client.request(3, "shutdown", "null");
    client.notification("exit", "null");
    assert_eq!(client.run(), 0);

    assert_eq!(client.pop()["id"], 1); // initialize reply
    client.pop(); // didOpen push
    let response = client.pop();
    assert_eq!(response["id"], 2);
    let value = response["result"]["contents"]["value"]
        .as_str()
        .expect("md");
    assert!(value.contains("token `α`"), "{value}");
}
