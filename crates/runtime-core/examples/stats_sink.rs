//! The kind-statistics seam as a consumer: one [`drive_sink`] pass over
//! a hand-built event stream feeds a [`TeeSink`] zipping
//! [`ValidationSink`] (the O(1) totals + balance verdict) with
//! [`StatsSink`] — whose final [`StreamStats`] snapshot reports the
//! per-kind histograms, nesting depth and token byte coverage. The
//! grammar/corpus profiling shape, without building a tree.
//!
//! Run: `cargo run -p runtime-core --example stats_sink`

use std::ops::Range;

use runtime_core::events::{Event, Output};
use runtime_core::sink::{StatsSink, TeeSink, ValidationSink, drive_sink};

fn main() {
    // A deliberately incomplete mini stream: `{ "a": ` — two nested
    // nodes, three tokens, one deferred error event (the missing value
    // after the colon). Kinds are pack-local dense numbers; the demo
    // picks arbitrary ones.
    let events = vec![
        Event::Start {
            kind: 10,
            forward_parent: None,
        },
        Event::Token { kind: 1, n_raw: 1 }, // `{`
        Event::Start {
            kind: 11,
            forward_parent: None,
        },
        Event::Token { kind: 2, n_raw: 1 }, // `"a"`
        Event::Token { kind: 3, n_raw: 1 }, // `:`
        Event::Error { diag: 0 },           // missing value (deferred index)
        Event::Finish,                      // closes kind 11
        Event::Finish,                      // closes kind 10
    ];
    let output: Output = runtime_core::events::process(&events).expect("stream validates");
    let ranges: Vec<Range<u32>> = vec![0..1, 1..4, 4..5];

    // One pass, two consumers: validation totals ride with the stats.
    let mut tee = TeeSink::new(ValidationSink::default(), StatsSink::default());
    drive_sink(&output, &ranges, &mut tee).expect("well-formed stream drives");

    let validation = tee.first.summary();
    let stats = tee.second.into_stats();
    println!(
        "validate: {} nodes, {} tokens, {} errors, balanced: {}",
        validation.nodes, validation.tokens, validation.errors, validation.balanced
    );
    println!(
        "stats: max_depth {}, token_bytes {} over the supplied ranges",
        stats.max_depth, stats.token_bytes
    );
    // Only the kinds the stream actually exercised (the histogram is
    // dense over the kinds seen; absent kinds read as zero).
    for (kind, count) in stats.node_histogram.iter().enumerate() {
        if *count == 0 {
            continue;
        }
        println!("  node kind {kind}: {count} enter(s)");
    }
    for (kind, count) in stats.token_histogram.iter().enumerate() {
        if *count == 0 {
            continue;
        }
        println!("  token kind {kind}: {count} token(s)");
    }

    assert!(
        validation.balanced,
        "every Start carries its closing Finish"
    );
    assert_eq!(stats.node_count(11), 1, "inner node counted by kind");
    assert_eq!(stats.token_count(2), 1);
    assert_eq!(stats.errors, 1);
    assert_eq!(stats.max_depth, 2, "two nested enters");
    assert_eq!(stats.token_bytes, 5, "ranges 0..1 + 1..4 + 4..5");
    println!("one pass: validation verdict + per-kind statistics");
}
