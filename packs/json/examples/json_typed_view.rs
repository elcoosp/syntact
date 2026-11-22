//! # Consumer example — typed views over a parsed JSON tree
//!
//! Demonstrates the §16.C.1 "typed views" default: a consumer drives the
//! JSON reference parser into a green tree, then projects typed accessors
//! (`Value`, `Object`, `Array`) over it without copying — building on the
//! `runtime_core::view::TreeView` seam rather than generated ASTs.
//!
//! Run with:
//!
//! ```sh
//! cargo run -p packs-json --example json_typed_view
//! ```

use packs_json::parse;
use packs_json::typed::{Array, Object, Value, ValueKind};
use runtime_core::input::Source;
use runtime_core::view::TreeView;
use runtime_core::view::visit::{Visitor, visit};
use std::ops::ControlFlow;

/// The fixture: a nested JSON document exercising every value kind.
const DOC: &str = r#"{
  "name": "syntact",
  "version": 1,
  "stable": true,
  "tags": ["parser", "toolkit", null],
  "deps": {"runtime-core": "*", "packs-json": "*"}
}"#;

/// A visitor that tallies every node and token leaf in a green tree — the
/// push counterpart to the `GreenCursor` pull walk.
struct Tally {
    /// Interior nodes entered.
    nodes: u32,
    /// Token leaves visited.
    tokens: u32,
}

impl Visitor for Tally {
    fn enter(&mut self, _: &runtime_core::tree::GreenNode) -> ControlFlow<()> {
        self.nodes += 1;
        ControlFlow::Continue(())
    }
    fn token(&mut self, _: runtime_core::TokenKind, _: std::ops::Range<u32>) -> ControlFlow<()> {
        self.tokens += 1;
        ControlFlow::Continue(())
    }
}

/// Walk a typed value, printing its kind and (for scalars) its bytes.
fn describe(indent: &str, key: Option<&str>, value: &Value<'_>) {
    let label = key.unwrap_or("-");
    match value.value_kind() {
        ValueKind::Object => {
            println!("{indent}{label}: object");
            for (k, v) in value.as_object().expect("object").pairs() {
                describe(&format!("{indent}  "), Some(k), &v);
            }
        }
        ValueKind::Array => {
            println!("{indent}{label}: array");
            let items: Array<'_> = value.as_array().expect("array");
            for (i, item) in items.items().iter().enumerate() {
                describe(&format!("{indent}  "), Some(&format!("[{i}]")), item);
            }
        }
        ValueKind::String => {
            println!("{indent}{label}: string {}", value.as_str().unwrap_or("?"));
        }
        ValueKind::Number => {
            println!(
                "{indent}{label}: number {}",
                value.as_number().unwrap_or("?")
            );
        }
        ValueKind::True => println!("{indent}{label}: true"),
        ValueKind::False => println!("{indent}{label}: false"),
        ValueKind::Null => println!("{indent}{label}: null"),
        ValueKind::Error => println!("{indent}{label}: <error region>"),
    }
}

fn main() {
    let tree = parse(DOC).expect("fixture parses losslessly");
    let source = Source::from_str_unchecked(DOC);
    let value = Value::new(&tree, &source);
    println!(
        "parsed JSON: root kind = {kind} (ROOT_KIND)",
        kind = value.kind()
    );
    println!("---");
    describe("", None, &value);
    println!("---");
    // Zero-copy contract: keys borrow the source text directly.
    let object: Object<'_> = value.as_object().expect("root is object");
    let keys: Vec<&str> = object.keys();
    println!("top-level keys ({n}): {keys:?}", n = keys.len());
    println!("---");
    // The visitor seam (push counterpart to the GreenCursor pull walk):
    // count every node and token in the green tree without a hand-rolled DFS.
    let mut tally = Tally {
        nodes: 0,
        tokens: 0,
    };
    let _ = visit(&tree, &mut tally);
    println!(
        "green tree tally: {nodes} nodes, {tokens} token leaves",
        nodes = tally.nodes,
        tokens = tally.tokens
    );
}
