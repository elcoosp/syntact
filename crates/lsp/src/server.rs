//! The server state machine (issue 041): workspace + LSP lifecycle +
//! the method dispatch table. [`Server::handle_message`] is a pure
//! function from one incoming message to the optional outgoing message
//! and a flow directive — the transport only moves bytes (the
//! playground-server split applied to stdio).

use serde::Deserialize;
use serde_json::Value;

use crate::analysis::{Diag, Workspace};
use crate::index::Position;
use crate::rpc::{
    self, INVALID_PARAMS, INVALID_REQUEST, Id, Incoming, METHOD_NOT_FOUND, PARSE_ERROR,
    SERVER_NOT_INITIALIZED,
};

/// What the transport should do after one message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// Keep serving.
    Continue,
    /// The client sent `exit`: stop.
    Exit,
}

/// Lifecycle states (LSP 3.17 initialization discipline).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Lifecycle {
    #[default]
    Uninitialized,
    Ready,
    ShutDown,
}

/// The language server: workspace state + lifecycle + dispatch.
#[derive(Debug, Default)]
pub struct Server {
    /// The analyzed documents.
    pub workspace: Workspace,
    lifecycle: Lifecycle,
    exit_code: i32,
}

/// The server's LSP capabilities advertisement.
#[must_use]
pub fn capabilities() -> Value {
    serde_json::json!({
        "capabilities": {
            "positionEncoding": "utf-16",
            "textDocumentSync": { "openClose": true, "change": 2 },
            "completionProvider": { "triggerCharacters": [] },
            "hoverProvider": true,
            "definitionProvider": true,
            "referencesProvider": true,
            "renameProvider": true,
            "documentSymbolProvider": true,
            "diagnosticProvider": {
                "interFileDependencies": true,
                "workspaceDiagnostics": false
            }
        },
        "serverInfo": { "name": "gir-lsp", "version": "1.0.0" }
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
#[serde(rename_all = "camelCase")]
struct DidOpenParams {
    text_document: TextDocWithText,
}

#[derive(Deserialize)]
struct TextDocWithText {
    uri: String,
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
#[serde(rename_all = "camelCase")]
struct ReferenceContext {
    include_declaration: bool,
}

#[derive(Deserialize)]
struct ReferenceParams {
    #[serde(rename = "textDocument")]
    doc: TextDoc,
    position: Position,
    context: ReferenceContext,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenameParams {
    #[serde(rename = "textDocument")]
    doc: TextDoc,
    position: Position,
    new_name: String,
}

fn parse_params<T: serde::de::DeserializeOwned>(params: Option<&Value>) -> Result<T, ()> {
    let value = params.ok_or(())?;
    serde_json::from_value(value.clone()).map_err(|_| ())
}

fn json_value<T: serde::Serialize>(value: T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

impl Server {
    /// A fresh, uninitialized server.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The exit code for `exit`: 0 after `shutdown`, 1 otherwise.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        self.exit_code
    }

    /// Handle one decoded message body; returns the outgoing message
    /// (a response or a push notification) and the flow directive.
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

    /// Requests: the dispatch table. Every arm produces the *result*
    /// (or an error code + message); the envelope is wrapped once.
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
            ("textDocument/hover", _) => self.position_query(params, |workspace, uri, offset| {
                offset
                    .and_then(|offset| workspace.hover(&uri, offset))
                    .map_or(Value::Null, json_value)
            }),
            ("textDocument/completion", _) => {
                self.position_query(params, |workspace, uri, offset| {
                    json_value(workspace.completion(
                        &uri,
                        offset.unwrap_or(usize::MAX), // no word under an empty position
                    ))
                })
            }
            ("textDocument/definition", _) => {
                self.position_query(params, |workspace, uri, offset| {
                    offset
                        .and_then(|offset| workspace.definition(&uri, offset))
                        .map_or(Value::Null, json_value)
                })
            }
            ("textDocument/references", _) => {
                self.typed_query(params, |workspace, p: ReferenceParams| {
                    let locations = workspace.offset_of(&p.doc.uri, p.position).map_or_else(
                        Vec::new,
                        |offset| {
                            workspace.references(&p.doc.uri, offset, p.context.include_declaration)
                        },
                    );
                    json_value(locations)
                })
            }
            ("textDocument/rename", _) => self.typed_query(params, |workspace, p: RenameParams| {
                let edit = workspace
                    .offset_of(&p.doc.uri, p.position)
                    .and_then(|offset| workspace.rename(&p.doc.uri, offset, &p.new_name).ok());
                edit.map_or(Value::Null, |edit| json_value(edit.changes))
            }),
            ("textDocument/documentSymbol", _) => self.uri_query(params, |workspace, uri| {
                json_value(workspace.document_symbols(&uri))
            }),
            ("textDocument/diagnostic", _) => self.uri_query(params, |workspace, uri| {
                json_value(serde_json::json!({
                    "kind": "full",
                    "items": workspace.diagnostics(&uri)
                }))
            }),
            _ => Err((METHOD_NOT_FOUND, "method not found")),
        };
        match outcome {
            Ok(result) => rpc::response_ok(id, result),
            Err((code, message)) => rpc::response_err(id, code, message),
        }
    }

    /// A query over a uri + position pair. Unknown uris and
    /// out-of-range positions hand `None` to the feature, which picks
    /// its own empty answer (hover `null`, completion `[]`, …).
    fn position_query<R>(
        &mut self,
        params: Option<&Value>,
        run: impl FnOnce(&Workspace, String, Option<usize>) -> R,
    ) -> Result<Value, (i64, &'static str)>
    where
        R: serde::Serialize,
    {
        self.typed_query(params, |workspace, p: PositionParams| {
            let offset = workspace
                .get(&p.doc.uri)
                .and_then(|document| document.line_index.offset(&document.text, p.position));
            run(workspace, p.doc.uri, offset)
        })
    }

    /// A query over a uri alone.
    fn uri_query<R>(
        &mut self,
        params: Option<&Value>,
        run: impl FnOnce(&Workspace, String) -> R,
    ) -> Result<Value, (i64, &'static str)>
    where
        R: serde::Serialize,
    {
        self.typed_query(params, |workspace, doc: UriOnlyParams| {
            run(workspace, doc.doc.uri)
        })
    }

    /// Deserialize the params (failure → `InvalidParams`) and run.
    fn typed_query<T, R>(
        &mut self,
        params: Option<&Value>,
        run: impl FnOnce(&Workspace, T) -> R,
    ) -> Result<Value, (i64, &'static str)>
    where
        T: serde::de::DeserializeOwned,
        R: serde::Serialize,
    {
        match parse_params::<T>(params) {
            Ok(typed) => Ok(json_value(run(&self.workspace, typed))),
            Err(()) => Err((INVALID_PARAMS, "invalid params")),
        }
    }

    /// Notifications: document sync + lifecycle. All diagnostics
    /// updates are pushed as `textDocument/publishDiagnostics`.
    fn handle_notification(
        &mut self,
        method: &str,
        params: Option<&Value>,
    ) -> (Option<Value>, Flow) {
        match (method, self.lifecycle) {
            ("exit", lifecycle) => {
                // Clean exit only after a successful `shutdown`.
                self.exit_code = i32::from(lifecycle != Lifecycle::ShutDown);
                (None, Flow::Exit)
            }
            // `initialized` and any other pre/post-sync notification is
            // dropped here (the catch-all below); the spec has no reply.
            (_, Lifecycle::Uninitialized | Lifecycle::ShutDown) => (None, Flow::Continue),
            ("textDocument/didOpen", _) => match parse_params::<DidOpenParams>(params) {
                Ok(params) => {
                    let uri = params.text_document.uri;
                    self.workspace.open(&uri, params.text_document.text);
                    let diagnostics = self.workspace.diagnostics(&uri);
                    (
                        Some(publish_diagnostics(&uri, &diagnostics)),
                        Flow::Continue,
                    )
                }
                Err(()) => (None, Flow::Continue),
            },
            ("textDocument/didChange", _) => match parse_params::<DidChangeParams>(params) {
                Ok(params) => {
                    let uri = params.text_document.uri;
                    // Edits apply in order; each range is resolved
                    // against the text state it applies to.
                    for change in &params.content_changes {
                        let span = change.range.and_then(|range| {
                            let document = self.workspace.get(&uri)?;
                            document.line_index.span(&document.text, range)
                        });
                        self.workspace.change(&uri, &[(span, &change.text)]);
                    }
                    let diagnostics = self.workspace.diagnostics(&uri);
                    (
                        Some(publish_diagnostics(&uri, &diagnostics)),
                        Flow::Continue,
                    )
                }
                Err(()) => (None, Flow::Continue),
            },
            ("textDocument/didClose", _) => match parse_params::<TextDoc>(params) {
                Ok(params) => {
                    self.workspace.close(&params.uri);
                    (Some(publish_diagnostics(&params.uri, &[])), Flow::Continue)
                }
                Err(()) => (None, Flow::Continue),
            },
            // Unknown notifications are dropped (spec).
            _ => (None, Flow::Continue),
        }
    }
}

fn publish_diagnostics(uri: &str, diagnostics: &[Diag]) -> Value {
    rpc::notification(
        "textDocument/publishDiagnostics",
        serde_json::json!({ "uri": uri, "diagnostics": diagnostics }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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

    const BROKEN: &str =
        "language \"brk\" version=\"1.0.0\"\ngrammar {\n    s kind=alt missing\n}\n";

    struct Harness {
        server: Server,
        next_id: i64,
        uri: String,
    }

    impl Harness {
        fn new() -> Self {
            let mut harness = Self {
                server: Server::new(),
                next_id: 1,
                uri: "file:///mini.gir".to_owned(),
            };
            let (out, flow) = harness
                .server
                .handle_message(br#"{"jsonrpc":"2.0","id":0,"method":"initialize"}"#);
            assert_eq!(flow, Flow::Continue);
            assert_eq!(
                out.expect("caps")["result"]["capabilities"]["hoverProvider"],
                json!(true)
            );
            harness
                .server
                .handle_message(br#"{"jsonrpc":"2.0","method":"initialized"}"#);
            harness.open(MINI);
            harness
        }

        fn request(&mut self, method: &str, params: &Value) -> Value {
            let id = self.next_id;
            self.next_id += 1;
            let body =
                format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{params}}}"#);
            let (out, flow) = self.server.handle_message(body.as_bytes());
            assert_eq!(flow, Flow::Continue);
            out.expect("response")
        }

        fn notify(&mut self, method: &str, params: &Value) -> Option<Value> {
            let body = format!(r#"{{"jsonrpc":"2.0","method":"{method}","params":{params}}}"#);
            let (out, flow) = self.server.handle_message(body.as_bytes());
            assert_eq!(flow, Flow::Continue);
            out
        }

        fn open(&mut self, text: &str) {
            let out = self.notify(
                "textDocument/didOpen",
                &json!({ "textDocument": { "uri": self.uri, "languageId": "gir", "version": 1, "text": text } }),
            );
            assert!(out.expect("publish")["params"]["diagnostics"].is_array());
        }

        fn pos(&self, text: &str, needle: &str) -> Value {
            let index = crate::index::LineIndex::new(text);
            let position = index.position(text, text.find(needle).expect("needle"));
            json!({ "textDocument": { "uri": self.uri }, "position": position })
        }
    }

    // -----------------------------------------------------------------
    // Lifecycle
    // -----------------------------------------------------------------

    #[test]
    fn initialize_advertises_capabilities_once() {
        let mut server = Server::new();
        let (out, _) = server.handle_message(br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#);
        let caps = out.expect("result")["result"]["capabilities"].clone();
        assert_eq!(caps["positionEncoding"], json!("utf-16"));
        assert_eq!(caps["textDocumentSync"]["change"], json!(2));
        assert_eq!(caps["hoverProvider"], json!(true));
        assert_eq!(caps["renameProvider"], json!(true));
        assert_eq!(
            caps["diagnosticProvider"]["interFileDependencies"],
            json!(true)
        );

        // `serverInfo` sits beside `capabilities` in InitializeResult;
        // a second `initialize` is an error.
        let (out, _) = server.handle_message(br#"{"jsonrpc":"2.0","id":2,"method":"initialize"}"#);
        assert_eq!(out.expect("err")["error"]["code"], json!(INVALID_REQUEST));
    }

    #[test]
    fn requests_before_initialize_are_rejected() {
        let mut server = Server::new();
        let (out, _) = server.handle_message(
            br#"{"jsonrpc":"2.0","id":5,"method":"textDocument/hover","params":{}}"#,
        );
        assert_eq!(
            out.expect("err")["error"]["code"],
            json!(SERVER_NOT_INITIALIZED)
        );
    }

    #[test]
    fn shutdown_then_exit_yields_zero() {
        let mut server = Server::new();
        server.handle_message(br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#);
        let (out, _) = server.handle_message(br#"{"jsonrpc":"2.0","id":2,"method":"shutdown"}"#);
        assert_eq!(out.expect("null")["result"], Value::Null);
        // Requests after shutdown are rejected.
        let (out, _) = server.handle_message(
            br#"{"jsonrpc":"2.0","id":3,"method":"textDocument/hover","params":{}}"#,
        );
        assert_eq!(out.expect("err")["error"]["code"], json!(INVALID_REQUEST));
        let (_, flow) = server.handle_message(br#"{"jsonrpc":"2.0","method":"exit"}"#);
        assert_eq!(flow, Flow::Exit);
        assert_eq!(server.exit_code(), 0);
    }

    #[test]
    fn exit_without_shutdown_yields_one() {
        let mut server = Server::new();
        server.handle_message(br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#);
        let (_, flow) = server.handle_message(br#"{"jsonrpc":"2.0","method":"exit"}"#);
        assert_eq!(flow, Flow::Exit);
        assert_eq!(server.exit_code(), 1);
    }

    #[test]
    fn malformed_bodies_get_parse_errors() {
        let mut server = Server::new();
        let (out, _) = server.handle_message(b"{not json");
        assert_eq!(out.expect("err")["error"]["code"], json!(PARSE_ERROR));
        let (out, _) = server.handle_message(br#"{"jsonrpc":"1.0"}"#);
        assert_eq!(out.expect("err")["error"]["code"], json!(INVALID_REQUEST));
    }

    #[test]
    fn unknown_methods_are_method_not_found_but_notifications_dropped() {
        let mut harness = Harness::new();
        let response = harness.request("textDocument/future", &json!({}));
        assert_eq!(response["error"]["code"], json!(METHOD_NOT_FOUND));
        assert!(
            harness
                .notify("workspace/didCreateFiles", &json!({}))
                .is_none()
        );
    }

    #[test]
    fn invalid_params_get_invalid_params_errors() {
        let mut harness = Harness::new();
        assert_eq!(
            harness.request("textDocument/hover", &json!({ "nope": true }))["error"]["code"],
            json!(INVALID_PARAMS)
        );
        assert_eq!(
            harness.request("textDocument/hover", &json!(null))["error"]["code"],
            json!(INVALID_PARAMS)
        );
        assert_eq!(
            harness.request("textDocument/documentSymbol", &json!({}))["error"]["code"],
            json!(INVALID_PARAMS)
        );
    }

    // -----------------------------------------------------------------
    // Document sync + diagnostics push
    // -----------------------------------------------------------------

    #[test]
    fn did_open_publishes_diagnostics() {
        let mut server = Server::new();
        server.handle_message(br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#);
        let (out, _) = server.handle_message(
            format!(
                r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"file:///x.gir","text":{}}}}}}}"#,
                serde_json::to_string(BROKEN).expect("json")
            )
            .as_bytes(),
        );
        let publish = out.expect("publish");
        assert_eq!(publish["method"], json!("textDocument/publishDiagnostics"));
        let items = publish["params"]["diagnostics"].clone();
        assert_ne!(items.as_array().expect("array").len(), 0);
        assert_eq!(items[0]["severity"], json!(1));
    }

    #[test]
    fn did_change_applies_incremental_edits_and_republishes() {
        let mut harness = Harness::new();
        // Break the grammar: `plus expr` → `plus wip` in `sum`'s body.
        let index = crate::index::LineIndex::new(MINI);
        let start = MINI.find("plus expr").expect("body") + "plus ".len();
        let range = index.range(
            MINI,
            gir_syntax::ByteSpan {
                start,
                end: start + 4,
            },
        );
        let out = harness
            .notify(
                "textDocument/didChange",
                &json!({
                    "textDocument": { "uri": harness.uri },
                    "contentChanges": [ { "range": range, "text": "wip" } ]
                }),
            )
            .expect("publish");
        let items = out["params"]["diagnostics"].as_array().expect("items");
        assert!(
            items
                .iter()
                .any(|item| item["message"].as_str().is_some_and(|m| m.contains("wip"))),
            "{items:?}"
        );

        // Fix it again (full-text change, no range).
        let out = harness
            .notify(
                "textDocument/didChange",
                &json!({
                    "textDocument": { "uri": harness.uri },
                    "contentChanges": [ { "text": MINI } ]
                }),
            )
            .expect("publish");
        let items = out["params"]["diagnostics"].as_array().expect("items");
        assert!(
            !items.iter().any(|item| item["severity"] == json!(1)),
            "{items:?}"
        );
    }

    // -----------------------------------------------------------------
    // Queries
    // -----------------------------------------------------------------

    #[test]
    fn hover_returns_markdown_contents() {
        let mut harness = Harness::new();
        let response = harness.request("textDocument/hover", &harness.pos(MINI, "sum    kind=seq"));
        let contents = &response["result"]["contents"];
        assert_eq!(contents["kind"], json!("markdown"));
        assert!(
            contents["value"]
                .as_str()
                .expect("text")
                .contains("rule `sum`")
        );
        // Hover over nothing → null.
        let brace_offset = MINI.find('{').expect("brace");
        let index = crate::index::LineIndex::new(MINI);
        let params = json!({
            "textDocument": { "uri": harness.uri },
            "position": index.position(MINI, brace_offset)
        });
        let response = harness.request("textDocument/hover", &params);
        assert_eq!(response["result"], Value::Null);
    }

    #[test]
    fn completion_returns_labeled_items() {
        let mut harness = Harness::new();
        let index = crate::index::LineIndex::new(MINI);
        // On the grammar block's closing brace: no word under the
        // cursor → the unfiltered list.
        let params = json!({
            "textDocument": { "uri": harness.uri },
            "position": index.position(MINI, MINI.rfind('}').expect("brace"))
        });
        let response = harness.request("textDocument/completion", &params);
        let items = response["result"].as_array().expect("items");
        let labels: Vec<&str> = items
            .iter()
            .filter_map(|item| item["label"].as_str())
            .collect();
        assert!(labels.contains(&"expr"));
        assert!(labels.contains(&"opt"));
    }

    #[test]
    fn definition_references_rename_document_symbols() {
        let mut harness = Harness::new();
        let index = crate::index::LineIndex::new(MINI);

        // definition from the `expr` reference inside `sum`.
        let ref_offset = MINI.find("plus expr").expect("ref") + "plus ".len();
        let params = json!({
            "textDocument": { "uri": harness.uri },
            "position": index.position(MINI, ref_offset)
        });
        let response = harness.request("textDocument/definition", &params);
        assert_eq!(response["result"]["uri"], json!(harness.uri));
        assert!(response["result"]["range"]["start"]["line"].is_u64());

        // references of the `expr` declaration (incl. declaration).
        let decl_offset = MINI.find("expr   kind=alt").expect("decl");
        let params = json!({
            "textDocument": { "uri": harness.uri },
            "position": index.position(MINI, decl_offset),
            "context": { "includeDeclaration": true }
        });
        let response = harness.request("textDocument/references", &params);
        let locations = response["result"].as_array().expect("locations");
        assert_eq!(locations.len(), 2);

        // rename `expr` → `expression`.
        let params = json!({
            "textDocument": { "uri": harness.uri },
            "position": index.position(MINI, decl_offset),
            "newName": "expression"
        });
        let response = harness.request("textDocument/rename", &params);
        let changes = &response["result"];
        let edits = changes[&harness.uri].as_array().expect("edits");
        assert_eq!(edits.len(), 2);
        assert!(
            edits
                .iter()
                .all(|edit| edit["newText"] == json!("expression"))
        );

        // document symbols.
        let params = json!({ "textDocument": { "uri": harness.uri } });
        let response = harness.request("textDocument/documentSymbol", &params);
        let symbols = response["result"].as_array().expect("symbols");
        assert_eq!(symbols.len(), 5);
        assert_eq!(symbols[2]["name"], json!("expr"));
        assert_eq!(symbols[2]["kind"], json!(12));
    }

    #[test]
    fn pull_diagnostics_wrap_items() {
        let mut harness = Harness::new();
        let response = harness.request(
            "textDocument/diagnostic",
            &json!({ "textDocument": { "uri": harness.uri } }),
        );
        assert_eq!(response["result"]["kind"], json!("full"));
        assert!(response["result"]["items"].is_array());
    }

    #[test]
    fn queries_on_unknown_uris_answer_empty_not_error() {
        let mut harness = Harness::new();
        let ghost = "file:///ghost.gir";
        let index = crate::index::LineIndex::new(MINI);
        let params = json!({
            "textDocument": { "uri": ghost },
            "position": index.position(MINI, 0)
        });
        assert_eq!(
            harness.request("textDocument/hover", &params)["result"],
            Value::Null
        );
        let params = json!({
            "textDocument": { "uri": ghost },
            "position": index.position(MINI, 0)
        });
        let response = harness.request("textDocument/completion", &params);
        assert_eq!(response["result"], json!([]));
        let response = harness.request(
            "textDocument/documentSymbol",
            &json!({ "textDocument": { "uri": ghost } }),
        );
        assert_eq!(response["result"], json!([]));
        // rename on a ghost uri → null result.
        let response = harness.request(
            "textDocument/rename",
            &json!({ "textDocument": { "uri": ghost }, "position": {"line": 0, "character": 0}, "newName": "x" }),
        );
        assert_eq!(response["result"], Value::Null);
    }
}
