//! End-to-end session tests (issue 042 TDD): a scripted client drives
//! the pack server (`transport::serve_with` over in-memory buffers)
//! through the full LSP lifecycle — initialize → didOpen → diagnostics
//! → queries → didChange → multi-document → didClose → shutdown →
//! exit — asserting every response on the wire.

use std::io::BufWriter;

use lsp::pack::server::PackServer;
use lsp::rpc::{self, FrameError};
use lsp::transport;
use serde_json::Value;

const JSON_DOC: &str = "{\n  \"alpha\": [1, true]\n}";
const TOML_DOC: &str = "[deps]\nname = \"syntact\"\n";

/// A scripted in-memory client: `send` queues a frame, and after
/// `serve_with` runs, `replies` decodes the responses in order.
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

    /// Run the pack server over the queued input and collect replies.
    fn run(&mut self) -> i32 {
        let mut server = PackServer::new();
        let mut output = Vec::new();
        let code = {
            let mut writer = BufWriter::new(&mut output);
            let mut reader = std::io::Cursor::new(self.input.clone());
            transport::serve_with(&mut server, &mut reader, &mut writer, 1 << 20)
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

fn position(text: &str, needle: &str) -> String {
    let index = lsp::LineIndex::new(text);
    serde_json::to_string(&index.position(text, text.find(needle).expect("needle")))
        .expect("position")
}

// One scripted narrative: splitting it would hide the protocol
// sequencing the test exists to pin.
#[test]
fn full_pack_session_lifecycle() {
    let mut client = Client::default();

    // 1. initialize — the pack-lsp capability set.
    client.request(1, "initialize", "{}");
    // 2. initialized (notification, no reply)
    client.notification("initialized", "{}");
    // 3. didOpen a JSON document (pushes diagnostics)
    client.notification(
        "textDocument/didOpen",
        &format!(
            r#"{{"textDocument":{{"uri":"file:///d.json","languageId":"json","version":1,"text":{}}}}}"#,
            serde_json::to_string(JSON_DOC).expect("json")
        ),
    );
    // 4. pull diagnostics: clean document, empty items
    client.request(
        2,
        "textDocument/diagnostic",
        r#"{"textDocument":{"uri":"file:///d.json"}}"#,
    );
    // 5. document symbols: object outline with the pair
    client.request(
        3,
        "textDocument/documentSymbol",
        r#"{"textDocument":{"uri":"file:///d.json"}}"#,
    );
    // 6. folding ranges: the object spans lines 0..=2
    client.request(
        4,
        "textDocument/foldingRange",
        r#"{"textDocument":{"uri":"file:///d.json"}}"#,
    );
    // 7. semantic tokens: fixed legend, delta-encoded data
    client.request(
        5,
        "textDocument/semanticTokens/full",
        r#"{"textDocument":{"uri":"file:///d.json"}}"#,
    );
    // 8. document highlight: the key string under the cursor
    client.request(
        6,
        "textDocument/documentHighlight",
        &format!(
            r#"{{"textDocument":{{"uri":"file:///d.json"}},"position":{}}}"#,
            position(JSON_DOC, "\"alpha\"")
        ),
    );
    // 9. selection range: the ancestor chain at the array element
    client.request(
        7,
        "textDocument/selectionRange",
        &format!(
            r#"{{"textDocument":{{"uri":"file:///d.json"}},"positions":[{}]}}"#,
            position(JSON_DOC, "1")
        ),
    );
    // 10. didChange: break the document (delete the array's bracket)
    client.notification(
        "textDocument/didChange",
        &format!(
            r#"{{"textDocument":{{"uri":"file:///d.json"}},"contentChanges":[{{"range":{{"start":{},"end":{}}},"text":""}}]}}"#,
            position(JSON_DOC, "]"),
            {
                let index = lsp::LineIndex::new(JSON_DOC);
                let mut end = index.position(JSON_DOC, JSON_DOC.find(']').expect("bracket"));
                end.character += 1;
                serde_json::to_string(&end).expect("pos")
            }
        ),
    );
    // 11. pull diagnostics: now the error region shows
    client.request(
        8,
        "textDocument/diagnostic",
        r#"{"textDocument":{"uri":"file:///d.json"}}"#,
    );
    // 12. didChange: repair it (re-insert the bracket)
    client.notification(
        "textDocument/didChange",
        &format!(
            r#"{{"textDocument":{{"uri":"file:///d.json"}},"contentChanges":[{{"range":{{"start":{},"end":{}}},"text":"]"}}]}}"#,
            position(JSON_DOC, "]"),
            position(JSON_DOC, "]")
        ),
    );
    // 13. a second document (toml, resolved by extension) — multi-file
    client.notification(
        "textDocument/didOpen",
        &format!(
            r#"{{"textDocument":{{"uri":"file:///d.toml","languageId":"plaintext","version":1,"text":{}}}}}"#,
            serde_json::to_string(TOML_DOC).expect("json")
        ),
    );
    // 14. symbols for the toml document (table outline)
    client.request(
        9,
        "textDocument/documentSymbol",
        r#"{"textDocument":{"uri":"file:///d.toml"}}"#,
    );
    // 15. didClose the json document (clears its diagnostics)
    client.notification(
        "textDocument/didClose",
        r#"{"textDocument":{"uri":"file:///d.json"}}"#,
    );
    // 16. shutdown (request) then exit (notification)
    client.request(10, "shutdown", "null");
    client.notification("exit", "null");

    let code = client.run();
    assert_eq!(code, 0, "clean shutdown exits 0");

    // 1. initialize capabilities
    let caps = client.pop()["result"]["capabilities"].clone();
    assert_eq!(caps["positionEncoding"], "utf-16");
    assert_eq!(caps["textDocumentSync"]["change"], 2);
    assert_eq!(caps["diagnosticProvider"]["workspaceDiagnostics"], false);
    assert_eq!(caps["documentSymbolProvider"], true);
    assert_eq!(caps["foldingRangeProvider"], true);
    assert_eq!(caps["selectionRangeProvider"], true);
    assert_eq!(caps["documentHighlightProvider"], true);
    assert_eq!(
        caps["semanticTokensProvider"]["legend"]["tokenTypes"][0],
        "string"
    );
    assert_eq!(
        caps["semanticTokensProvider"]["legend"]["tokenTypes"][1],
        "number"
    );
    assert_eq!(
        caps["semanticTokensProvider"]["legend"]["tokenTypes"][2],
        "operator"
    );
    assert_eq!(
        caps["semanticTokensProvider"]["legend"]["tokenTypes"][3],
        "identifier"
    );

    // 3. didOpen pushed the (empty) diagnostics for the json document
    let publish = client.pop();
    assert_eq!(publish["method"], "textDocument/publishDiagnostics");
    assert_eq!(publish["params"]["uri"], "file:///d.json");
    assert_eq!(publish["params"]["diagnostics"], Value::Array(vec![]));

    // 4. pull diagnostics: empty for the clean document
    let response = client.pop();
    assert_eq!(response["id"], 2);
    assert_eq!(response["result"]["kind"], "full");
    assert_eq!(response["result"]["items"], Value::Array(vec![]));

    // 5. document symbols: the object, pair named after the key
    let response = client.pop();
    assert_eq!(response["id"], 3);
    let symbols = response["result"].as_array().expect("symbols");
    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0]["name"], "{");
    assert_eq!(symbols[0]["children"][0]["name"], "\"alpha\"");
    assert_eq!(symbols[0]["children"][0]["children"][0]["name"], "[");
    assert_eq!(
        symbols[0]["children"][0]["selectionRange"]["start"]["line"],
        1
    );

    // 6. folding: root and object span the document, the pair's span
    // starts in the trailing trivia of line 0 and ends on line 1; the
    // array lives on one line and folds nothing.
    let response = client.pop();
    assert_eq!(response["id"], 4);
    let folds = response["result"].as_array().expect("folds");
    assert_eq!(folds.len(), 3);
    assert_eq!(folds[0]["startLine"], 0);
    assert_eq!(folds[0]["endLine"], 2);
    assert_eq!(folds[1]["startLine"], 0);
    assert_eq!(folds[1]["endLine"], 2);
    assert_eq!(folds[2]["startLine"], 0);
    assert_eq!(folds[2]["endLine"], 1);

    // 7. semantic tokens: every token classified, deltas consistent
    let response = client.pop();
    assert_eq!(response["id"], 5);
    let data = response["result"]["data"].as_array().expect("data");
    assert_eq!(data.len() % 5, 0);
    let classes: Vec<i64> = data
        .iter()
        .skip(3)
        .step_by(5)
        .map(|value| value.as_i64().expect("class"))
        .collect();
    assert!(classes.contains(&0), "string class present");
    assert!(classes.contains(&1), "number class present");
    assert!(classes.contains(&2), "operator class present");
    assert!(classes.contains(&3), "identifier class present");

    // 8. highlight: exactly one occurrence (single key)
    let response = client.pop();
    assert_eq!(response["id"], 6);
    let highlights = response["result"].as_array().expect("highlights");
    assert_eq!(highlights.len(), 1);
    assert_eq!(highlights[0]["kind"], 1);

    // 9. selection range: nested ancestors
    let response = client.pop();
    assert_eq!(response["id"], 7);
    let selection = &response["result"][0];
    assert!(selection["parent"]["parent"]["parent"]["range"].is_object());

    // 10. didChange (break) pushed the error diagnostics
    let publish = client.pop();
    assert_eq!(publish["method"], "textDocument/publishDiagnostics");
    assert_eq!(publish["params"]["uri"], "file:///d.json");
    let pushed = publish["params"]["diagnostics"].as_array().expect("items");
    // The recovery produces three ERROR regions for the unterminated
    // array followed by the stray object closer (deterministic).
    assert_eq!(pushed.len(), 3);
    assert!(pushed.iter().all(|item| item["message"] == "syntax error"));

    // 11. the diagnostic pull (scripted between the two changes)
    // reports the broken document: three deterministic ERROR regions.
    let response = client.pop();
    assert_eq!(response["id"], 8);
    let items = response["result"]["items"].as_array().expect("items");
    assert_eq!(items.len(), 3);
    assert!(items.iter().all(|item| item["message"] == "syntax error"));

    // 12. didChange (repair) pushed empty diagnostics again
    let publish = client.pop();
    assert_eq!(publish["method"], "textDocument/publishDiagnostics");
    assert_eq!(publish["params"]["uri"], "file:///d.json");
    assert_eq!(publish["params"]["diagnostics"], Value::Array(vec![]));

    // 13. the toml document opened (extension fallback): a push
    let publish = client.pop();
    assert_eq!(publish["method"], "textDocument/publishDiagnostics");
    assert_eq!(publish["params"]["uri"], "file:///d.toml");
    assert_eq!(publish["params"]["diagnostics"], Value::Array(vec![]));

    // 14. toml symbols: the table (named by its header's name token —
    // document-order first significant), then the assignment.
    let response = client.pop();
    assert_eq!(response["id"], 9);
    let symbols = response["result"].as_array().expect("symbols");
    assert_eq!(symbols.len(), 2);
    assert_eq!(symbols[0]["name"], "deps");
    assert_eq!(symbols[0]["children"][0]["name"], "deps");
    assert_eq!(symbols[1]["name"], "name");
    assert_eq!(symbols[1]["children"][0]["name"], "name");

    // 15. didClose clears the diagnostics
    let publish = client.pop();
    assert_eq!(publish["method"], "textDocument/publishDiagnostics");
    assert_eq!(publish["params"]["uri"], "file:///d.json");
    assert_eq!(publish["params"]["diagnostics"], Value::Array(vec![]));

    // 16. shutdown result
    let response = client.pop();
    assert_eq!(response["id"], 10);
    assert_eq!(response["result"], Value::Null);

    assert_eq!(
        client.replies,
        Vec::<Value>::new(),
        "every frame accounted for"
    );
}

#[test]
fn utf16_positions_round_trip_through_the_wire() {
    let doc = "{\n  \"clé\": \"valeur 😀\"\n}";
    let mut client = Client::default();
    client.request(1, "initialize", "{}");
    client.notification(
        "textDocument/didOpen",
        &format!(
            r#"{{"textDocument":{{"uri":"file:///u.json","languageId":"json","version":1,"text":{}}}}}"#,
            serde_json::to_string(doc).expect("json")
        ),
    );
    // Highlight the key string: its position counts UTF-16 units.
    client.request(
        2,
        "textDocument/documentHighlight",
        &format!(
            r#"{{"textDocument":{{"uri":"file:///u.json"}},"position":{}}}"#,
            position(doc, "clé")
        ),
    );
    client.run();
    client.pop(); // initialize
    client.pop(); // diagnostics push
    let response = client.pop();
    let highlights = response["result"].as_array().expect("highlights");
    assert_eq!(highlights.len(), 1);
    // The highlight range decodes back to the key's byte span.
    let index = lsp::LineIndex::new(doc);
    let start: lsp::Position =
        serde_json::from_value(highlights[0]["range"]["start"].clone()).expect("position");
    let offset = index.offset(doc, start).expect("in range");
    assert!(doc[offset..].starts_with("\"clé\""));
}

#[test]
fn unknown_language_documents_are_ignored_cleanly() {
    let mut client = Client::default();
    client.request(1, "initialize", "{}");
    client.notification(
        "textDocument/didOpen",
        r#"{"textDocument":{"uri":"file:///x.yaml","languageId":"yaml","version":1,"text":"a: 1"}}"#,
    );
    client.request(
        2,
        "textDocument/documentSymbol",
        r#"{"textDocument":{"uri":"file:///x.yaml"}}"#,
    );
    client.request(3, "shutdown", "null");
    client.notification("exit", "null");
    assert_eq!(client.run(), 0);
    client.pop(); // initialize
    // No diagnostics push (nothing analyzed), then the query.
    let response = client.pop();
    assert_eq!(response["id"], 2);
    assert_eq!(response["result"], Value::Array(vec![]));
    client.pop(); // shutdown
    assert_eq!(client.replies, Vec::<Value>::new());
}
