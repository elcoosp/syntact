//! The diagnostic-resolution seam as a consumer: one [`drive_sink`]
//! pass over a hand-built event stream builds the green tree (the
//! [`TeeSink`] first half) AND resolves the stream's deferred diag
//! indexes into owned, stream-ordered diagnostics (the
//! [`DiagCollector`] second half) — the tree-and-diagnostics shape
//! every error-reporting host needs, without a second pass.
//!
//! Run: `cargo run -p runtime-core --example diag_collector`

use std::ops::Range;
use std::sync::Arc;

use runtime_core::diagnostic::DiagTable;
use runtime_core::events::{Event, Output};
use runtime_core::input::Source;
use runtime_core::sink::{DiagCollector, TeeSink, drive_sink};
use runtime_core::tree::{GreenNode, NodeCache, TreeBuilder};

fn main() {
    // A deliberately malformed mini stream: `{ "a": ` — the value
    // token after the colon is missing, so the parser emits an error
    // event (deferred: the stream carries only the index 0). A second
    // error index (7) sits beyond the side table to show the
    // unresolved-count probe.
    let source = r#"{"a":"#;
    let events = vec![
        Event::Start {
            kind: 10,
            forward_parent: None,
        },
        Event::Token { kind: 1, n_raw: 1 }, // `{`
        Event::Token { kind: 2, n_raw: 1 }, // `"a"`
        Event::Token { kind: 3, n_raw: 1 }, // `:`
        Event::Error { diag: 0 },           // missing value (in table)
        Event::Error { diag: 7 },           // beyond the table
        Event::Finish,
    ];
    let output: Output = runtime_core::events::process(&events).expect("stream validates");
    let ranges: Vec<Range<u32>> = vec![0..1, 1..4, 4..5];

    // The side table the indexes resolve into (the parse produces it
    // alongside the stream; here it backs the demo stream).
    let mut table = DiagTable::default();
    table.push("missing value after colon", 4..5);

    // One pass, two consumers: the tree builder and the diagnostic
    // resolver ride the same drive.
    let input = Source::from_str_unchecked(source);
    let cache = Arc::new(NodeCache::new());
    let mut tee = TeeSink::new(
        TreeBuilder::with_source(cache, &input),
        DiagCollector::new(&table),
    );
    drive_sink(&output, &ranges, &mut tee).expect("well-formed stream drives");

    // Tree half: byte-faithful despite the error events.
    let built: Arc<GreenNode> = tee.first.finish().expect("single root");
    assert_eq!(built.render(&input), source, "lossless");
    println!("tree: renders byte-for-byte ({source:?})");

    // Diagnostics half: stream order, owned messages + ranges.
    let collector = tee.second;
    for diagnostic in collector.diagnostics() {
        println!("  diag at {:?}: {}", diagnostic.range, diagnostic.message);
    }
    assert_eq!(collector.diagnostics().len(), 1, "only index 0 resolved");
    assert_eq!(collector.unresolved(), 1, "index 7 counted, not resolved");
    println!("one pass: tree + diagnostics, unresolved counted");
}
