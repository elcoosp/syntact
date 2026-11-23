//! Example: tree-level changed ranges after an edit (§7.7 downstream
//! invalidation).
//!
//! Parse a JSON document, apply an edit, reparse, and walk the two green
//! trees: [`changed_tree_ranges`] reports the coalesced byte ranges where
//! the trees disagree, and a counting [`DiffVisitor`] shows the full
//! Matches/MayDiffer/Differs classification of one pass.
//!
//! Run: `cargo run -p incremental --example tree_diff`

use std::sync::Arc;

use incremental::{DiffVisitor, RangeCollector, SubtreeVerdict, changed_tree_ranges, diff_trees};
use packs_json::parse;
use runtime_core::input::Source;
use runtime_core::tree::GreenNode;

/// Per-verdict walk counters (the observability convention: plain counts).
#[derive(Debug, Default)]
struct Counter {
    matches: u32,
    may_differ: u32,
    differs: u32,
}

impl DiffVisitor for Counter {
    fn subtree(
        &mut self,
        verdict: SubtreeVerdict,
        _old_span: std::ops::Range<u32>,
        _new_span: std::ops::Range<u32>,
    ) -> std::ops::ControlFlow<()> {
        match verdict {
            SubtreeVerdict::Matches => self.matches += 1,
            SubtreeVerdict::MayDiffer => self.may_differ += 1,
            SubtreeVerdict::Differs => self.differs += 1,
        }
        std::ops::ControlFlow::Continue(())
    }
}

fn main() {
    let old_source = r#"{"name": "syntact", "tags": ["fast", "lossless"], "n": 42}"#;
    // Insert one element into the tags array.
    let new_source = r#"{"name": "syntact", "tags": ["fast", "lossless", "incremental"], "n": 42}"#;

    let old: Arc<GreenNode> = parse(old_source).expect("lossless parse");
    let new: Arc<GreenNode> = parse(new_source).expect("lossless parse");
    assert_eq!(
        old.render(&Source::from_str_unchecked(old_source)),
        old_source
    );
    assert_eq!(
        new.render(&Source::from_str_unchecked(new_source)),
        new_source
    );

    // The built-in product: coalesced new-source byte ranges.
    let ranges = changed_tree_ranges(&old, old_source, &new, new_source);
    println!("tree-level changed ranges: {ranges:?}");
    for range in &ranges {
        let snippet = &new_source[((range.start as usize).min(new_source.len()))
            ..((range.end as usize).min(new_source.len()))];
        println!("  {range:?} = {snippet:?}");
    }

    // The same walk observed event-by-event through a custom visitor.
    let mut counter = Counter::default();
    let flow = diff_trees(&old, old_source, &new, new_source, &mut counter);
    println!("walk verdicts: {counter:?} (flow {flow:?})");

    // And the collector form of the same drive.
    let mut collector = RangeCollector::new();
    let _ = diff_trees(&old, old_source, &new, new_source, &mut collector);
    println!("collector ranges: {:?}", collector.ranges());
}
