//! # Consumer example — the generated JSON parser (issue 045)
//!
//! Demonstrates the codegen-rust descent backend's emitted output as a
//! real consumer would use it: parse a document through the generated
//! crate (regenerated on every build by `generated-json`'s build.rs),
//! count the errors recovery absorbed, and walk the green tree.
//!
//! The generated parser reproduces `descent::engine` output byte-for-byte
//! (the three-way parity gate asserts it) while reusing the
//! `runtime-core` event/tree substrates and the `lexgen` scanner — no
//! second lexer engine, no forked runtime.
//!
//! Run with:
//!
//! ```sh
//! cargo run -p bench --example generated_json
//! ```

use runtime_core::input::Source;
use runtime_core::view::GreenCursor;
use runtime_core::view::WalkEvent;

/// A well-formed fixture exercising every JSON value kind.
const DOC: &str = r#"{
  "name": "syntact",
  "version": 1,
  "stable": true,
  "tags": ["parser", "toolkit", null],
  "deps": {"runtime-core": "*", "packs-json": "*"}
}"#;

/// A malformed fixture: recovery absorbs the stray `}` and stays lossless.
const BAD_DOC: &str = "{ \"a\": [1, 2}, \"b\": 0 }";

fn main() {
    // The generated parser's API mirrors packs-json: `parse_events` for the
    // raw stream, `parse` for the green tree.
    let (events, tokens, errors) = generated_json::parse_events(DOC);
    println!(
        "generated parser: {} events, {} tokens, {} errors (valid doc)",
        events.len(),
        tokens.len(),
        errors
    );
    assert_eq!(errors, 0, "valid doc has no errors");

    let tree = generated_json::parse(DOC).expect("tree integration never fails structurally");
    let source = Source::from_str_unchecked(DOC);
    let rendered = tree.render(&source);
    assert_eq!(rendered, DOC, "render(parse(x)) == x (byte-faithful)");
    println!(
        "byte-faithful: render(parse(DOC)) == DOC ({} bytes)",
        DOC.len()
    );

    // Walk the tree with a GreenCursor (the rowan-style pull traversal).
    let mut cursor = GreenCursor::new(&tree);
    let mut enters = 0_u32;
    let mut leaves = 0_u32;
    while let Some(event) = cursor.next_event() {
        match event {
            WalkEvent::Enter(_) => enters += 1,
            WalkEvent::Leave(_) => {}
            WalkEvent::Token { .. } => leaves += 1,
        }
    }
    println!("tree walk: {enters} nodes entered, {leaves} token leaves");

    // The malformed doc: recovery keeps the result lossless.
    let (_, _, bad_errors) = generated_json::parse_events(BAD_DOC);
    let bad_tree =
        generated_json::parse(BAD_DOC).expect("recovery keeps the tree structurally valid");
    let bad_source = Source::from_str_unchecked(BAD_DOC);
    let bad_rendered = bad_tree.render(&bad_source);
    assert_eq!(
        bad_rendered, BAD_DOC,
        "recovery is lossless: render(parse(bad)) == bad"
    );
    println!(
        "recovery: {bad_errors} error(s) absorbed, render(parse(BAD_DOC)) == BAD_DOC (lossless)"
    );

    println!("\nThe generated crate is the codegen-rust descent backend's output,");
    println!("regenerated on every `cargo build` via generated-json/build.rs — the");
    println!("same source `syntact build json.gir -o <dir>` emits.");
}
