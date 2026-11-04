//! Issue 036 TDD: persistent-red version retention, structural
//! sharing measured by `Arc::ptr_eq`, the no-`&mut` read API, and
//! deterministic parallel merge order on seeded corpora.

use std::ops::Range;
use std::sync::Arc;

use runtime_core::events::{Output, Step};
use runtime_core::input::Source;
use runtime_core::parallel::{bytes_per_chunk, hits_per_chunk, map_chunks};
use runtime_core::persistent::{
    VersionLog, branch_covering, shared_arcs, shared_bytes, splice_child,
};
use runtime_core::sink::TriviaRecord;
use runtime_core::tree::{GreenChild, GreenNode, NodeCache, TreeBuilder};

/// Build a ROOT tree with `n` node children (statements), each a
/// `kind 7` node holding one `kind 1` token of `text`; token ranges
/// start at `offset` (spliced children must point into the composite
/// source they are spliced into).
fn build_root_at(offset: u32, texts: &[&str]) -> Arc<GreenNode> {
    let mut steps = vec![Step::Enter { kind: 100 }];
    let mut ranges: Vec<Range<u32>> = Vec::new();
    let mut trivia: Vec<TriviaRecord> = Vec::new();
    let mut cursor = offset;
    let mut source = String::new();
    for (idx, text) in texts.iter().enumerate() {
        steps.push(Step::Enter { kind: 7 });
        if idx > 0 {
            source.push('\n');
            let gap = cursor;
            cursor += 1;
            trivia.push(TriviaRecord {
                kind: 14,
                range: gap..cursor,
                trailing: true,
            });
        }
        let start = cursor;
        source.push_str(text);
        cursor += u32::try_from(text.len()).unwrap_or(u32::MAX);
        ranges.push(start..cursor);
        steps.push(Step::Token { kind: 1, n_raw: 1 });
        steps.push(Step::Exit);
    }
    steps.push(Step::Exit);
    let src = Source::from_str_unchecked(&source);
    let mut stream = Vec::new();
    for step in &steps {
        match *step {
            Step::Enter { kind } => stream.push(Output::pack_enter(kind)),
            Step::Exit => stream.push(Output::pack_exit()),
            Step::Token { kind, n_raw } => stream.push(Output::pack_token(kind, n_raw)),
            Step::Error { diag } => stream.push(Output::pack_error(diag)),
        }
    }
    #[allow(clippy::arc_with_non_send_sync)]
    let cache = Arc::new(NodeCache::new());
    let mut builder = TreeBuilder::with_source(cache, &src);
    builder.attach_trivia_owned(trivia);
    runtime_core::sink::drive_sink(&Output { stream }, &ranges, &mut builder).expect("drives");
    builder.finish().expect("single root")
}

fn child(root: &GreenNode, idx: usize) -> Arc<GreenNode> {
    match &root.children[idx] {
        GreenChild::Node(n) => Arc::clone(n),
        GreenChild::Token { .. } => panic!("expected a node child"),
    }
}

#[test]
fn version_retention_across_edits() {
    let v0_root = build_root_at(0, &["alpha", "beta", "gamma"]);
    let mut log = VersionLog::new();
    let v0 = log.commit(None, Arc::clone(&v0_root));

    // Edit: replace "beta" with "BETA" — a same-length, in-place COW
    // splice of branch 1 (the splice contract: length-preserving
    // replacements keep every sibling's absolute ranges valid; the
    // incremental engine's fragment reuse covers length changes).
    // The replacement carries its leading gap leaf (the ADR-008
    // trivia between the statements lives inside the next child).
    let replacement = build_root_at(5, &["\nBETA"]);
    let new_root = splice_child(&v0_root, 1, &replacement);
    let v1 = log.commit(Some(&v0), new_root);

    // Edit 2 branches off v1 ("delta" replaces "gamma", same length).
    let replacement2 = build_root_at(10, &["\ndelta"]);
    let v2_root = splice_child(v1.root(), 2, &replacement2);
    let v2 = log.commit(Some(&v1), v2_root);

    // Retention: v0 and v1 are still resolvable and intact.
    assert_eq!(log.len(), 3);
    let kept0 = log.version(0).expect("v0 retained");
    let kept1 = log.version(1).expect("v1 retained");
    assert_eq!(kept0.version(), 0);
    assert_eq!(kept1.parent(), Some(0));
    assert_eq!(v2.parent(), Some(1));
    // The old handle renders its own (stale) content — persistent.
    let src0 = Source::from_str_unchecked("alpha\nbeta\ngamma");
    assert_eq!(kept0.render(&src0), "alpha\nbeta\ngamma");
    let src1 = Source::from_str_unchecked("alpha\nBETA\ngamma");
    assert_eq!(kept1.render(&src1), "alpha\nBETA\ngamma");
    let src2 = Source::from_str_unchecked("alpha\nBETA\ndelta");
    assert_eq!(v2.root().render(&src2), "alpha\nBETA\ndelta");
    // The head is the newest version.
    assert_eq!(log.head().expect("head").version(), 2);
}

#[test]
fn splice_shares_untouched_subtrees() {
    let root = build_root_at(0, &["alpha", "beta", "gamma"]);
    let old0 = child(&root, 0);
    let old1 = child(&root, 1);
    let old2 = child(&root, 2);

    // Splice branch 1 only (same length, in place; the replacement
    // carries its leading gap leaf).
    let replacement = build_root_at(5, &["\nBETA"]);
    let new_root = splice_child(&root, 1, &replacement);

    // Untouched branches are pointer-identical (Arc reuse measured).
    let new0 = child(&new_root, 0);
    let new2 = child(&new_root, 2);
    assert!(Arc::ptr_eq(&old0, &new0), "branch 0 shared");
    assert!(Arc::ptr_eq(&old2, &new2), "branch 2 shared");
    let new1 = child(&new_root, 1);
    assert!(!Arc::ptr_eq(&old1, &new1), "branch 1 replaced");
    // Shared subtree counts: 2 shared children.
    assert_eq!(shared_arcs(&root, &new_root), 2);
    // Shared bytes cover branches 0 and 2 (plus their inner tokens —
    // the walk counts every ptr_eq pair along the path).
    assert!(shared_bytes(&root, &new_root) >= old0.text_len + old2.text_len);
}

#[test]
fn read_api_takes_no_mutable_access() {
    // The read path compiles against a shared (&) log and (&) handle:
    // single-writer / multi-reader by construction.
    let mut log = VersionLog::new();
    let root = build_root_at(0, &["one", "two"]);
    let handle = log.commit(None, root);
    let reader = &handle; // an immutable alias
    let src = Source::from_str_unchecked("one\ntwo");
    assert_eq!(reader.render(&src), "one\ntwo");
    assert_eq!(reader.root().kind, 100);
    // The writer can still commit while readers hold old handles.
    let replacement = build_root_at(0, &["ONE"]);
    let _v1 = log.commit(Some(&handle), splice_child(handle.root(), 0, &replacement));
    assert_eq!(reader.version(), 0, "old handle is unaffected by commit");
}

#[test]
fn branch_covering_finds_the_splice_point() {
    // "alpha"(5) gap(1) "beta"(4) gap(1) "gamma"(5).
    let root = build_root_at(0, &["alpha", "beta", "gamma"]);
    assert_eq!(branch_covering(&root, 0), Some(0));
    assert_eq!(branch_covering(&root, 6), Some(1)); // inside "beta"
    assert_eq!(branch_covering(&root, 11), Some(2)); // inside "gamma"
    assert_eq!(branch_covering(&root, 99), None);
}

#[test]
fn parallel_merge_order_is_deterministic_on_seeded_corpus() {
    // Seeded corpus: an LCG generates 4096 pseudo-random tokens over
    // a SoA table; the chunk plan is 8 balanced chunks.
    let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
    let mut lcg = move || {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) as u32
    };
    let mut ranges: Vec<Range<u32>> = Vec::new();
    let mut kinds: Vec<u16> = Vec::new();
    let mut cursor = 0_u32;
    for _ in 0..4096 {
        let len = 1 + lcg() % 12;
        ranges.push(cursor..cursor + len);
        kinds.push(u16::try_from(lcg() % 8).unwrap_or(0));
        cursor += len;
    }
    let chunks: Vec<Range<u32>> = (0..8)
        .map(|i| {
            let step = cursor / 8;
            let start = i * step;
            let end = if i == 7 { cursor } else { (i + 1) * step };
            start..end
        })
        .collect();

    // The parallel result equals the sequential reference — order and
    // content — and repeats identically across runs.
    let want = 3_u16;
    let expected_sequential: Vec<usize> = chunks
        .iter()
        .map(|c| {
            ranges
                .iter()
                .zip(&kinds)
                .filter(|(r, _)| {
                    let center = r.start + (r.end - r.start) / 2;
                    c.start <= center && center < c.end
                })
                .filter(|(_, k)| **k == want)
                .count()
        })
        .collect();
    for _ in 0..5 {
        let got = hits_per_chunk(&chunks, &kinds, &ranges, want);
        assert_eq!(got, expected_sequential, "parallel == sequential");
        let bytes = bytes_per_chunk(&chunks, &ranges);
        let total: u32 = bytes.iter().sum();
        assert_eq!(
            total,
            ranges.iter().map(|r| r.end - r.start).sum::<u32>(),
            "byte coverage conserved across the merge"
        );
    }
    // map_chunks: identity probe — output index == chunk order.
    let order: Vec<u32> = map_chunks(&chunks, |c| c.start);
    let starts: Vec<u32> = chunks.iter().map(|c| c.start).collect();
    assert_eq!(order, starts, "merge order follows chunk position");
}

#[test]
fn parallel_mode_scales_chunks_to_workers() {
    // More chunks than workers: every chunk still gets exactly one
    // slot, in order (the single-writer merge contract).
    let chunks: Vec<Range<u32>> = (0..17).map(|i| (i * 10)..((i + 1) * 10)).collect();
    let out = map_chunks(&chunks, |c| c.start * 2);
    assert_eq!(out.len(), 17);
    assert_eq!(out[16], 320);
    // One chunk: inline path (no worker spawn).
    let single: Vec<Range<u32>> = std::iter::once(0..10).collect();
    assert_eq!(map_chunks(&single, |c| c.end), vec![10]);
    // Empty plan: empty result.
    let empty: Vec<Range<u32>> = Vec::new();
    let none = map_chunks::<u32, _>(&empty, |_| 1);
    assert_eq!(none.len(), 0);
}
