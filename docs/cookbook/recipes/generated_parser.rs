// Recipe: parse JSON with the generated parser crate (issue 045).
//
// The codegen-rust descent backend emits a standalone, compiling Rust
// parser crate from a `.gir` grammar. This recipe drives the emitted
// crate (the `generated-json` workspace member, regenerated on every
// build) the way a real consumer would: parse a document, count the
// errors recovery absorbed, and verify byte-faithfulness.
//
// The emitted parser reproduces `descent::engine` output byte-for-byte
// (the three-way parity gate asserts it) while reusing `runtime-core`
// (events/trees) and `lexgen` (scanner) — no second lexer engine, no
// forked runtime. `syntact build json.gir -o <dir>` emits this same
// source for any grammar that lowers cleanly.

use runtime_core::input::Source;

#[test]
fn generated_parser_is_byte_faithful() {
    let source = r#"{"name": "syntact", "version": 1, "ok": true}"#;
    let (events, tokens, errors) = generated_json::parse_events(source);
    assert_eq!(errors, 0, "valid doc has no errors");
    assert!(!events.is_empty(), "events emitted");
    assert!(!tokens.is_empty(), "tokens emitted");

    let tree = generated_json::parse(source).expect("tree integration never fails structurally");
    let input = Source::from_str_unchecked(source);
    assert_eq!(tree.render(&input), source, "render(parse(x)) == x");
}

#[test]
fn generated_parser_recovery_is_lossless() {
    // A stray `}` mid-array: recovery absorbs it and keeps the tree
    // byte-faithful (every unplaceable token lands in an ERROR node).
    let source = "{ \"a\": [1, 2}, \"b\": 0 }";
    let (_, _, errors) = generated_json::parse_events(source);
    assert!(errors > 0, "recovery reported the error");
    let tree = generated_json::parse(source).expect("recovery keeps the tree structurally valid");
    let input = Source::from_str_unchecked(source);
    assert_eq!(tree.render(&input), source, "recovery is lossless");
}
