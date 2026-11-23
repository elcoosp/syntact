//! Criterion suite for the tree-level changed-ranges diff (§7.7): the
//! old/new green-tree walk on identical parses (the all-Matches cost)
//! and on a single-token edit, head-to-head against the token-level
//! brute-force diff over the same parses. Run-fast config — trends only.

use std::sync::Arc;

use criterion::{Criterion, criterion_group, criterion_main};
use incremental::changed_tree_ranges;
use incremental::diff::changed_ranges as token_changed_ranges;
use packs_json::parse;
use runtime_core::tree::GreenNode;

fn tree_diff(c: &mut Criterion) {
    let old_source = bench::steady_state_document();
    let old: Arc<GreenNode> = parse(&old_source).expect("lossless parse");
    // Same-length single-token edit: the first `1.5` becomes `2.5`.
    let anchor = "\"k1\": 1.5";
    let at = old_source
        .find(anchor)
        .expect("steady-state fixture carries the anchor");
    let new_source = format!(
        "{}{}{}",
        &old_source[..at],
        "\"k1\": 2.5",
        &old_source[at + anchor.len()..]
    );
    let new: Arc<GreenNode> = parse(&new_source).expect("lossless parse");
    let old_tokens = old.token_ranges();
    let new_tokens = new.token_ranges();

    let mut group = c.benchmark_group("tree_diff");
    group.bench_function("identical_parses", |b| {
        b.iter(|| {
            let ranges = changed_tree_ranges(&old, &old_source, &old, &old_source);
            debug_assert!(ranges.is_empty(), "identical parses never change");
            ranges
        });
    });
    group.bench_function("single_token_edit", |b| {
        b.iter(|| changed_tree_ranges(&old, &old_source, &new, &new_source));
    });
    group.bench_function("token_level_brute_force", |b| {
        b.iter(|| token_changed_ranges(&old_tokens, &old_source, &new_tokens, &new_source));
    });
    group.finish();
}

criterion_group!(
    name = tree_diff_suites;
    config = Criterion::default()
        .sample_size(20)
        .warm_up_time(std::time::Duration::from_millis(200))
        .measurement_time(std::time::Duration::from_secs(2));
    targets = tree_diff
);
criterion_main!(tree_diff_suites);
