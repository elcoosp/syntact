//! The pack server state machine (issue 042): workspace, LSP lifecycle
//! and dispatch, mirroring the grammar server's discipline. The
//! `handle_message` method is a pure function from one incoming message
//! to the optional outgoing message and a flow directive; the shared
//! transport only moves bytes.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use crate::analysis::Diag;
use crate::index::Position;
use crate::pack::document::{Document, DocumentHighlight, FoldingRange, PackSymbol};
use crate::pack::registry::PackRegistry;
use crate::rpc::{
    self, INVALID_PARAMS, INVALID_REQUEST, Id, Incoming, METHOD_NOT_FOUND, PARSE_ERROR,
    SERVER_NOT_INITIALIZED,
};
use crate::server::Flow;

/// The semantic-token legend the capabilities advertise (documented
/// shape classes — see [`crate::pack::document::classify_token`]).
pub const SEMANTIC_TOKEN_TYPES: [&str; 4] = crate::pack::document::SEMANTIC_TOKEN_TYPES;

/// The server's LSP capabilities advertisement.
#[must_use]
pub fn capabilities() -> Value {
    serde_json::json!({
        "capabilities": {
            "positionEncoding": "utf-16",
            "textDocumentSync": { "openClose": true, "change": 2 },
            "diagnosticProvider": {
                "interFileDependencies": false,
                "workspaceDiagnostics": false
            },
            "documentSymbolProvider": true,
            "foldingRangeProvider": true,
            "selectionRangeProvider": true,
            "documentHighlightProvider": true,
            "semanticTokensProvider": {
                "legend": { "tokenTypes": SEMANTIC_TOKEN_TYPES, "tokenModifiers": [] },
                "full": true
            }
        },
        "serverInfo": { "name": "pack-lsp", "version": "1.0.0" }
    })
}

// ---------------------------------------------------------------------------
// Incoming params (camelCase on the wire)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct TextDoc {
    uri: String,
}

/// `{ "textDocument": { "uri": … } }` — the uri-only params shape.
#[derive(Deserialize)]
struct UriOnlyParams {
    #[serde(rename = "textDocument")]
    doc: TextDoc,
}

#[derive(Deserialize)]
struct DidOpenParams {
    #[serde(rename = "textDocument")]
    doc: OpenDoc,
}

#[derive(Deserialize)]
struct OpenDoc {
    uri: String,
    #[serde(rename = "languageId")]
    language_id: String,
    text: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContentChange {
    range: Option<crate::index::Range>,
    text: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DidChangeParams {
    text_document: TextDoc,
    content_changes: Vec<ContentChange>,
}

#[derive(Deserialize)]
struct PositionParams {
    #[serde(rename = "textDocument")]
    doc: TextDoc,
    position: Position,
}

#[derive(Deserialize)]
struct SelectionRangeParams {
    #[serde(rename = "textDocument")]
    doc: TextDoc,
    positions: Vec<Position>,
}

fn parse_params<T: serde::de::DeserializeOwned>(params: Option<&Value>) -> Result<T, ()> {
    let value = params.ok_or(())?;
    serde_json::from_value(value.clone()).map_err(|_| ())
}

fn json_value<T: serde::Serialize>(value: T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// One nested selection range (LSP `SelectionRange` shape: the parent
/// chain forms the ancestor spans).
#[derive(serde::Serialize)]
struct SelectionRange {
    range: crate::index::Range,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent: Option<Box<SelectionRange>>,
}

/// The pack language server: registry + documents + lifecycle.
#[derive(Debug, Default)]
pub struct PackServer {
    /// The pack registry (defaults to [`PackRegistry::reference`]).
    pub registry: PackRegistry,
    /// The analyzed documents.
    pub documents: BTreeMap<String, Document>,
    lifecycle: Lifecycle,
    exit_code: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Lifecycle {
    #[default]
    Uninitialized,
    Ready,
    ShutDown,
}

impl PackServer {
    /// A fresh server over the reference registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            registry: PackRegistry::reference(),
            documents: BTreeMap::new(),
            lifecycle: Lifecycle::Uninitialized,
            exit_code: 0,
        }
    }

    /// A fresh server over a custom registry (test seam).
    #[must_use]
    pub fn with_registry(registry: PackRegistry) -> Self {
        Self {
            registry,
            ..Self::new()
        }
    }

    /// The exit code for `exit`: 0 after `shutdown`, 1 otherwise.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        self.exit_code
    }

    /// Handle one decoded message body.
    pub fn handle_message(&mut self, body: &[u8]) -> (Option<Value>, Flow) {
        match rpc::parse_incoming(body) {
            Err(rpc::RpcError::Parse) => (
                Some(rpc::response_err(
                    &Id::Num(0),
                    PARSE_ERROR,
                    "invalid JSON body",
                )),
                Flow::Continue,
            ),
            Err(rpc::RpcError::Shape) => (
                Some(rpc::response_err(
                    &Id::Num(0),
                    INVALID_REQUEST,
                    "malformed JSON-RPC envelope",
                )),
                Flow::Continue,
            ),
            Ok(Incoming::Request { id, method, params }) => (
                Some(self.handle_request(&id, &method, params.as_ref())),
                Flow::Continue,
            ),
            Ok(Incoming::Notification { method, params }) => {
                self.handle_notification(&method, params.as_ref())
            }
        }
    }

    fn handle_request(&mut self, id: &Id, method: &str, params: Option<&Value>) -> Value {
        let outcome: Result<Value, (i64, &'static str)> = match (method, self.lifecycle) {
            ("initialize", Lifecycle::Uninitialized) => {
                self.lifecycle = Lifecycle::Ready;
                Ok(capabilities())
            }
            ("initialize", _) => Err((INVALID_REQUEST, "server already initialized")),
            (_, Lifecycle::Uninitialized) => {
                Err((SERVER_NOT_INITIALIZED, "server not initialized"))
            }
            (_, Lifecycle::ShutDown) => Err((INVALID_REQUEST, "server shut down")),
            ("shutdown", _) => {
                self.lifecycle = Lifecycle::ShutDown;
                Ok(Value::Null)
            }
            ("textDocument/diagnostic", _) => self.uri_query(params, |server, uri| {
                serde_json::json!({
                    "kind": "full",
                    "items": server.diagnostics(&uri),
                })
            }),
            ("textDocument/documentSymbol", _) => {
                self.uri_query(params, |server, uri| json_value(server.symbols(&uri)))
            }
            ("textDocument/foldingRange", _) => {
                self.uri_query(params, |server, uri| json_value(server.folding(&uri)))
            }
            ("textDocument/selectionRange", _) => {
                self.typed_query(params, |server, p: SelectionRangeParams| {
                    json_value(server.selection_ranges(&p.doc.uri, &p.positions))
                })
            }
            ("textDocument/documentHighlight", _) => {
                self.position_query(params, |server, uri, offset| {
                    json_value(server.highlight(&uri, offset.unwrap_or(usize::MAX)))
                })
            }
            ("textDocument/semanticTokens/full", _) => self.uri_query(
                params,
                |server, uri| serde_json::json!({ "data": server.semantic_data(&uri) }),
            ),
            _ => Err((METHOD_NOT_FOUND, "method not found")),
        };
        match outcome {
            Ok(result) => rpc::response_ok(id, result),
            Err((code, message)) => rpc::response_err(id, code, message),
        }
    }

    fn handle_notification(
        &mut self,
        method: &str,
        params: Option<&Value>,
    ) -> (Option<Value>, Flow) {
        match (method, self.lifecycle) {
            ("exit", lifecycle) => {
                self.exit_code = i32::from(lifecycle != Lifecycle::ShutDown);
                (None, Flow::Exit)
            }
            (_, Lifecycle::Uninitialized | Lifecycle::ShutDown) => (None, Flow::Continue),
            ("textDocument/didOpen", _) => match parse_params::<DidOpenParams>(params) {
                Ok(params) => {
                    let uri = params.doc.uri;
                    let pack = self
                        .registry
                        .for_language(&params.doc.language_id)
                        .or_else(|| self.registry.for_uri(&uri));
                    if let Some(entry) = pack {
                        let document =
                            Document::open(&uri, entry.name, entry.parse, &params.doc.text);
                        let diagnostics = document.diagnostics.clone();
                        self.documents.insert(uri.clone(), document);
                        return (
                            Some(publish_diagnostics(&uri, &diagnostics)),
                            Flow::Continue,
                        );
                    }
                    // No pack for this language: nothing to analyze, no
                    // diagnostics to push.
                    (None, Flow::Continue)
                }
                Err(()) => (None, Flow::Continue),
            },
            ("textDocument/didChange", _) => match parse_params::<DidChangeParams>(params) {
                Ok(params) => {
                    let uri = params.text_document.uri;
                    self.change(&uri, &params.content_changes);
                    let diagnostics = self.diagnostics(&uri).to_vec();
                    (
                        Some(publish_diagnostics(&uri, &diagnostics)),
                        Flow::Continue,
                    )
                }
                Err(()) => (None, Flow::Continue),
            },
            ("textDocument/didClose", _) => match parse_params::<UriOnlyParams>(params) {
                Ok(params) => {
                    self.documents.remove(&params.doc.uri);
                    (
                        Some(publish_diagnostics(&params.doc.uri, &[])),
                        Flow::Continue,
                    )
                }
                Err(()) => (None, Flow::Continue),
            },
            // Unknown notifications are dropped (spec).
            _ => (None, Flow::Continue),
        }
    }

    /// Apply one didChange batch; the pushed diagnostics are the
    /// document's rebuilt view.
    fn change(&mut self, uri: &str, changes: &[ContentChange]) {
        let Some(document) = self.documents.get_mut(uri) else {
            return;
        };
        let parse = self.registry.get(document.pack).map_or_else(
            || packs_json::parse as crate::pack::registry::ParseFn,
            |entry| entry.parse,
        );
        for change in changes {
            let span = change
                .range
                .and_then(|range| document.line_index.span(&document.text, range));
            let span = span.map(|span| span.start..span.end);
            document.edit(parse, &[(span, &change.text)]);
        }
    }

    /// Diagnostics of one document (borrowed — the query paths serve
    /// the prebuilt views without cloning them).
    fn diagnostics(&self, uri: &str) -> &[Diag] {
        self.documents
            .get(uri)
            .map_or(&[], |document| document.diagnostics.as_slice())
    }

    fn symbols(&self, uri: &str) -> &[PackSymbol] {
        self.documents
            .get(uri)
            .map_or(&[], |document| document.symbols.as_slice())
    }

    fn folding(&self, uri: &str) -> &[FoldingRange] {
        self.documents
            .get(uri)
            .map_or(&[], |document| document.folding.as_slice())
    }

    fn semantic_data(&self, uri: &str) -> &[u32] {
        self.documents
            .get(uri)
            .map_or(&[], |document| document.semantic_data.as_slice())
    }

    fn highlight(&self, uri: &str, offset: usize) -> Vec<DocumentHighlight> {
        self.documents
            .get(uri)
            .map_or_else(Vec::new, |document| document.highlight(offset))
    }

    fn selection_ranges(&self, uri: &str, positions: &[Position]) -> Vec<SelectionRange> {
        let Some(document) = self.documents.get(uri) else {
            return Vec::new();
        };
        positions
            .iter()
            .filter_map(|position| {
                let offset = document.line_index.offset(&document.text, *position)?;
                let chain = document.selection_chain(offset);
                // Build the parent chain outward: the innermost range is
                // the answer, every wider span becomes its parent.
                let mut current: Option<SelectionRange> = None;
                for range in chain.into_iter().rev() {
                    current = Some(SelectionRange {
                        range,
                        parent: current.map(Box::new),
                    });
                }
                current
            })
            .collect()
    }

    // -- shared dispatch helpers -------------------------------------------

    fn position_query<R>(
        &mut self,
        params: Option<&Value>,
        run: impl FnOnce(&Self, String, Option<usize>) -> R,
    ) -> Result<Value, (i64, &'static str)>
    where
        R: serde::Serialize,
    {
        self.typed_query(params, |server, p: PositionParams| {
            let offset = server
                .documents
                .get(&p.doc.uri)
                .and_then(|document| document.line_index.offset(&document.text, p.position));
            run(server, p.doc.uri, offset)
        })
    }

    fn uri_query<R>(
        &mut self,
        params: Option<&Value>,
        run: impl FnOnce(&Self, String) -> R,
    ) -> Result<Value, (i64, &'static str)>
    where
        R: serde::Serialize,
    {
        self.typed_query(params, |server, doc: UriOnlyParams| {
            run(server, doc.doc.uri)
        })
    }

    fn typed_query<T, R>(
        &mut self,
        params: Option<&Value>,
        run: impl FnOnce(&Self, T) -> R,
    ) -> Result<Value, (i64, &'static str)>
    where
        T: serde::de::DeserializeOwned,
        R: serde::Serialize,
    {
        match parse_params::<T>(params) {
            Ok(typed) => Ok(json_value(run(self, typed))),
            Err(()) => Err((INVALID_PARAMS, "invalid params")),
        }
    }
}

fn publish_diagnostics(uri: &str, diagnostics: &[Diag]) -> Value {
    rpc::notification(
        "textDocument/publishDiagnostics",
        serde_json::json!({ "uri": uri, "diagnostics": diagnostics }),
    )
}

impl crate::transport::Handler for PackServer {
    fn handle_message(&mut self, body: &[u8]) -> (Option<Value>, Flow) {
        PackServer::handle_message(self, body)
    }

    fn exit_code(&self) -> i32 {
        PackServer::exit_code(self)
    }
}

/// Serve a pack LSP session over `reader`/`writer` (the CLI entry).
///
/// # Errors
/// Never returns `Err`; the process exit code carries the verdict.
pub fn serve(
    reader: &mut impl std::io::BufRead,
    writer: &mut impl std::io::Write,
    max_message: usize,
) -> i32 {
    crate::transport::serve_with(&mut PackServer::new(), reader, writer, max_message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const DOC: &str = "{\n  \"alpha\": [1, true]\n}";
    const BROKEN: &str = "{\"a\": }";

    struct Harness {
        server: PackServer,
        next_id: i64,
        uri: String,
    }

    impl Harness {
        fn new() -> Self {
            let mut harness = Self {
                server: PackServer::new(),
                next_id: 1,
                uri: "file:///doc.json".to_owned(),
            };
            let (out, _) = harness
                .server
                .handle_message(br#"{"jsonrpc":"2.0","id":0,"method":"initialize"}"#);
            assert_eq!(
                out.expect("caps")["result"]["capabilities"]["semanticTokensProvider"]["legend"]["tokenTypes"]
                    [0],
                json!("string")
            );
            harness.open(DOC, "json");
            harness
        }

        fn request(&mut self, method: &str, params: &Value) -> Value {
            let id = self.next_id;
            self.next_id += 1;
            let body =
                format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{params}}}"#);
            let (out, _) = self.server.handle_message(body.as_bytes());
            out.expect("response")
        }

        fn notify(&mut self, method: &str, params: &Value) -> Option<Value> {
            let body = format!(r#"{{"jsonrpc":"2.0","method":"{method}","params":{params}}}"#);
            let (out, _) = self.server.handle_message(body.as_bytes());
            out
        }

        fn open(&mut self, text: &str, language: &str) -> Value {
            self.notify(
                "textDocument/didOpen",
                &json!({ "textDocument": { "uri": self.uri, "languageId": language, "version": 1, "text": text } }),
            )
            .expect("publish")
        }

        fn pos(&self, needle: &str) -> Value {
            let document = self.server.documents.get(&self.uri).expect("open");
            let index = crate::index::LineIndex::new(&document.text);
            let position =
                index.position(&document.text, document.text.find(needle).expect("needle"));
            json!({ "textDocument": { "uri": self.uri }, "position": position })
        }
    }

    #[test]
    fn lifecycle_matches_the_grammar_server_discipline() {
        let mut server = PackServer::new();
        // Queries before initialize are refused.
        let (out, _) = server.handle_message(
            br#"{"jsonrpc":"2.0","id":1,"method":"textDocument/foldingRange","params":{"textDocument":{"uri":"x"}}}"#,
        );
        assert_eq!(
            out.expect("error")["error"]["code"],
            json!(rpc::SERVER_NOT_INITIALIZED)
        );
        // Double initialize is refused.
        server.handle_message(br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#);
        let (out, _) = server.handle_message(br#"{"jsonrpc":"2.0","id":2,"method":"initialize"}"#);
        assert_eq!(
            out.expect("error")["error"]["code"],
            json!(rpc::INVALID_REQUEST)
        );
        // Exit without shutdown is unclean.
        let (out, flow) = server.handle_message(br#"{"jsonrpc":"2.0","method":"exit"}"#);
        assert!(out.is_none());
        assert_eq!(flow, Flow::Exit);
        assert_eq!(server.exit_code(), 1);
        // A clean shutdown exits 0.
        let mut server = PackServer::new();
        server.handle_message(br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#);
        server.handle_message(br#"{"jsonrpc":"2.0","id":2,"method":"shutdown"}"#);
        let (out, flow) = server.handle_message(br#"{"jsonrpc":"2.0","method":"exit"}"#);
        assert!(out.is_none());
        assert_eq!(flow, Flow::Exit);
        assert_eq!(server.exit_code(), 0);
        // Post-shutdown requests are refused.
        let (out, _) = server.handle_message(
            br#"{"jsonrpc":"2.0","id":3,"method":"textDocument/foldingRange","params":{"textDocument":{"uri":"x"}}}"#,
        );
        assert_eq!(
            out.expect("error")["error"]["code"],
            json!(rpc::INVALID_REQUEST)
        );
    }

    #[test]
    fn did_open_pushes_diagnostics_and_resolves_by_language() {
        let mut harness = Harness::new();
        // The harness opened a clean document: no diagnostics.
        let publish = harness.open(DOC, "json");
        assert_eq!(publish["params"]["diagnostics"], json!([]));
        // Malformed content pushes error-region diagnostics.
        let publish = harness.open(BROKEN, "json");
        let diagnostics = publish["params"]["diagnostics"].as_array().expect("items");
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0]["severity"], json!(1));
        assert_eq!(diagnostics[0]["message"], json!("syntax error"));
    }

    #[test]
    fn did_open_resolves_by_uri_extension_when_language_unknown() {
        let mut server = PackServer::new();
        server.handle_message(br#"{"jsonrpc":"2.0","id":0,"method":"initialize"}"#);
        let (out, _) = server.handle_message(
            br#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///cfg.toml","languageId":"plaintext","version":1,"text":"[a]\nb = 1"}}}"#,
        );
        // The uri extension resolved the toml pack and pushed diagnostics.
        assert!(out.is_some(), "extension fallback analyzes the document");
        assert!(server.documents.contains_key("file:///cfg.toml"));
        // Unknown language and extension: no document is stored.
        let (out, _) = server.handle_message(
            br#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///x.yaml","languageId":"yaml","version":1,"text":"a: 1"}}}"#,
        );
        assert!(out.is_none());
        assert!(!server.documents.contains_key("file:///x.yaml"));
    }

    #[test]
    fn did_change_rebuilds_and_pushes_diagnostics() {
        let mut harness = Harness::new();
        let index = crate::index::LineIndex::new(DOC);
        // Break the document: delete the array's closing bracket.
        let start = index.position(
            &harness.server.documents[harness.uri.as_str()].text,
            DOC.len() - 3,
        );
        let end = index.position(
            &harness.server.documents[harness.uri.as_str()].text,
            DOC.len() - 2,
        );
        let publish = harness
            .notify(
                "textDocument/didChange",
                &json!({
                    "textDocument": { "uri": harness.uri },
                    "contentChanges": [{ "range": { "start": start, "end": end }, "text": "" }]
                }),
            )
            .expect("publish");
        let diagnostics = publish["params"]["diagnostics"].as_array().expect("items");
        assert!(!diagnostics.is_empty(), "breaking the doc reports errors");
        // Repair it: diagnostics clear.
        let publish = harness
            .notify(
                "textDocument/didChange",
                &json!({
                    "textDocument": { "uri": harness.uri },
                    "contentChanges": [{ "range": { "start": start, "end": start }, "text": "]" }]
                }),
            )
            .expect("publish");
        let diagnostics = publish["params"]["diagnostics"].as_array().expect("items");
        assert!(diagnostics.is_empty(), "repairing the doc clears errors");
    }

    #[test]
    fn did_change_full_replacement_and_unknown_uri() {
        let mut harness = Harness::new();
        let publish = harness
            .notify(
                "textDocument/didChange",
                &json!({
                    "textDocument": { "uri": harness.uri },
                    "contentChanges": [{ "text": "[1, 2" }]
                }),
            )
            .expect("publish");
        assert_eq!(
            publish["params"]["diagnostics"]
                .as_array()
                .expect("items")
                .len(),
            1
        );
        // Unknown uri: no crash, empty push.
        let publish = harness
            .notify(
                "textDocument/didChange",
                &json!({
                    "textDocument": { "uri": "file:///ghost.json" },
                    "contentChanges": [{ "text": "[]" }]
                }),
            )
            .expect("publish");
        assert_eq!(publish["params"]["diagnostics"], json!([]));
    }

    #[test]
    fn query_methods_serve_the_prebuilt_views() {
        let mut harness = Harness::new();
        // Diagnostics (pull).
        let response = harness.request(
            "textDocument/diagnostic",
            &json!({ "textDocument": { "uri": harness.uri } }),
        );
        assert_eq!(response["result"]["kind"], json!("full"));
        // Document symbols: root excluded, object at top with the pair.
        let response = harness.request(
            "textDocument/documentSymbol",
            &json!({ "textDocument": { "uri": harness.uri } }),
        );
        let symbols = response["result"].as_array().expect("symbols");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0]["name"], json!("{"));
        assert_eq!(symbols[0]["children"][0]["name"], json!("\"alpha\""));
        assert_eq!(
            symbols[0]["children"][0]["selectionRange"]["start"]["line"],
            json!(1)
        );
        // Folding: the object spans lines 0..=2.
        let response = harness.request(
            "textDocument/foldingRange",
            &json!({ "textDocument": { "uri": harness.uri } }),
        );
        let folds = response["result"].as_array().expect("folds");
        assert_eq!(folds[0]["startLine"], json!(0));
        assert_eq!(folds[0]["endLine"], json!(2));
        // Semantic tokens: string, number, identifier classes present.
        let response = harness.request(
            "textDocument/semanticTokens/full",
            &json!({ "textDocument": { "uri": harness.uri } }),
        );
        let data = response["result"]["data"].as_array().expect("data");
        assert_eq!(data.len() % 5, 0);
        let classes: Vec<i64> = data
            .iter()
            .skip(3)
            .step_by(5)
            .map(|v| v.as_i64().expect("u32"))
            .collect();
        assert!(classes.contains(&0) && classes.contains(&1) && classes.contains(&3));
        // Highlight: the string under the cursor matches itself.
        let response = harness.request("textDocument/documentHighlight", &harness.pos("\"alpha\""));
        assert_eq!(response["result"].as_array().expect("items").len(), 1);
        // Selection range: chain from innermost to root.
        let response = harness.request(
            "textDocument/selectionRange",
            &json!({
                "textDocument": { "uri": harness.uri },
                "positions": [harness.pos("1")["position"].clone()]
            }),
        );
        let selection = &response["result"][0];
        assert!(selection["parent"].is_object(), "chain has ancestors");
        assert!(selection["parent"]["parent"]["range"].is_object());
        // Unknown uri queries answer empty.
        let response = harness.request(
            "textDocument/documentSymbol",
            &json!({ "textDocument": { "uri": "file:///ghost.json" } }),
        );
        assert_eq!(response["result"], json!([]));
    }

    #[test]
    fn malformed_params_and_unknown_methods_are_refused() {
        let mut harness = Harness::new();
        let response = harness.request(
            "textDocument/documentSymbol",
            &json!({ "textDocument": 42 }),
        );
        assert_eq!(response["error"]["code"], json!(rpc::INVALID_PARAMS));
        let response = harness.request("textDocument/formatting", &json!({}));
        assert_eq!(response["error"]["code"], json!(rpc::METHOD_NOT_FOUND));
        let (out, _) = harness.server.handle_message(b"not json");
        assert_eq!(
            out.expect("error")["error"]["code"],
            json!(rpc::PARSE_ERROR)
        );
    }

    #[test]
    fn did_close_clears_the_document() {
        let mut harness = Harness::new();
        let out = harness
            .notify(
                "textDocument/didClose",
                &json!({ "textDocument": { "uri": harness.uri } }),
            )
            .expect("publish");
        assert_eq!(out["params"]["diagnostics"], json!([]));
        assert!(harness.server.documents.is_empty());
    }
}
