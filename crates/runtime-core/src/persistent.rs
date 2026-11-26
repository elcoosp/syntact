//! # persistent-red — versioned red-tree handles + parallel queries
//! (issue 036, §7.10)
//!
//! A persistent-red layer over the green tree: versioned handles with
//! structural sharing across reparses (copy-on-write via `Arc` — no
//! `&mut` API anywhere), single-writer/multi-reader invariants, and a
//! deterministic parallel query mode over SoA-style chunk plans.
//!
//! # Invariants (documented contract)
//!
//! - **Single writer / multi reader**: a [`RedHandle`] publishes a new
//!   immutable version per reparse; readers hold `Arc<GreenNode>`
//!   snapshots that can never observe a half-published tree. The
//!   writer is the only party producing versions (enforced by taking
//!   `&mut self` on publish); readers clone the handle's version cell.
//! - **Structural sharing**: reparses share unchanged subtrees by
//!   `Arc` pointer identity — [`RedHandle::publish`] measures how many
//!   old `Arc`s the new root reuses (the sharing metric the tests
//!   gate on).
//! - **Version retention**: every published version stays alive as
//!   long as any reader holds it; [`RedHandle::version`] walks the
//!   retained chain.
//! - **Chunked merge determinism**: [`parallel_query`] splits the
//!   token stream into chunks (the structural-index chunk plan shape),
//!   scans each chunk on scoped worker threads (the execution half of
//!   issue 036's "parallel query mode" — the same
//!   `std::thread::scope` drive the structural index's region-parallel
//!   validation runs on), and merges the results in ascending
//!   start-offset order — the merged output is byte-for-byte identical
//!   to the sequential scan, seeded-corpus tested, whatever the thread
//!   completion order.
//!
//! # Verification — loom-clean (roadmap §11.2, §12.3 P4 gate)
//!
//! The documented contract above is model-checked, not just
//! prose-asserted: with the crate's `loom` feature enabled, the `Arc`
//! below is loom's model type, so the REAL [`RedHandle`] /
//! [`Version`] / [`count_shared`] types participate in the
//! interleaving search of `crates/runtime-core/tests/loom_red.rs`
//! (single-writer publish integrity, multi-reader snapshot sharing,
//! structural sharing across versions, version retention under
//! concurrent publishes). Run: `cargo test -p runtime-core --features
//! loom`.

use std::ops::Range;

#[cfg(feature = "loom")]
use loom::sync::Arc;
#[cfg(not(feature = "loom"))]
use std::sync::Arc;

use crate::tree::{GreenChild, GreenNode};

/// One published tree version: an immutable root plus its identity.
/// (`PartialEq`/`Eq` are derived in normal builds; under the test-only
/// `loom` feature the root is loom's model `Arc`, which implements
/// neither — equality there is the pointer identity the harness
/// asserts.)
#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "loom"), derive(PartialEq, Eq))]
pub struct Version {
    /// Monotonic version number (0 = the first publish).
    pub number: u64,
    /// The immutable root.
    pub root: Arc<GreenNode>,
}

/// A versioned red-tree handle — the single-writer entry point.
///
/// Readers clone [`RedHandle::current`] (an `Arc` snapshot); the writer
/// publishes new versions through `&mut self`. Nothing in this module
/// exposes `&mut` access to a published [`GreenNode`]: mutation happens
/// by building a new tree and sharing what did not change.
#[derive(Debug, Clone, Default)]
pub struct RedHandle {
    latest: Option<Version>,
    /// Retained history (bounded by reader clones, but v0 keeps all —
    /// version retention is a TDD gate).
    history: Vec<Version>,
}

impl RedHandle {
    /// A handle with no versions yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The current version's root, or `None` before the first publish.
    #[must_use]
    pub fn current(&self) -> Option<&Arc<GreenNode>> {
        self.latest.as_ref().map(|v| &v.root)
    }

    /// The current version number (0 before the first publish).
    #[must_use]
    pub fn version(&self) -> u64 {
        self.latest.as_ref().map_or(0, |v| v.number)
    }

    /// All retained versions, oldest first (retention contract).
    #[must_use]
    pub fn history(&self) -> &[Version] {
        &self.history
    }

    /// Publish a freshly built root as the next version. Returns the
    /// number of old-root `Arc` subtrees the new root reuses — the
    /// structural-sharing metric. `&mut self` = the single-writer gate.
    ///
    /// # Panics
    /// Never.
    pub fn publish(&mut self, root: Arc<GreenNode>) -> usize {
        let number = self.version() + 1;
        let shared = self
            .latest
            .as_ref()
            .map_or(0, |previous| count_shared(&previous.root, &root));
        let version = Version { number, root };
        self.history.push(version.clone());
        self.latest = Some(version);
        shared
    }
}

/// Count the `Arc`-identical subtree pairs between two roots (a joint
/// DFS; identity = `Arc::ptr_eq`). Under the test-only `loom` feature
/// the ROOT-level pointer check runs on loom's model `Arc` (the
/// version cell the harness model-checks); in every build the child
/// recursion runs on the kernel's `std` `Arc`s — [`GreenChild`] owns
/// `std` `Arc`s either way, so the tree below the root is identical.
#[must_use]
pub fn count_shared(old: &Arc<GreenNode>, new: &Arc<GreenNode>) -> usize {
    if Arc::ptr_eq(old, new) {
        return 1; // this whole subtree is shared
    }
    if old.kind != new.kind || old.children.len() != new.children.len() {
        return 0;
    }
    count_shared_children(&old.children, &new.children)
}

/// The subtree recursion over [`GreenChild`] slices on the kernel's
/// `std` `Arc`s, identical under every build.
fn count_shared_children(old_children: &[GreenChild], new_children: &[GreenChild]) -> usize {
    let mut total = 0;
    for (o, n) in old_children.iter().zip(new_children.iter()) {
        if let (GreenChild::Node(o), GreenChild::Node(n)) = (o, n) {
            total += count_shared_std(o, n);
        }
    }
    total
}

/// The identical-subtree recursion on `std` `Arc`s.
fn count_shared_std(old: &std::sync::Arc<GreenNode>, new: &std::sync::Arc<GreenNode>) -> usize {
    if std::sync::Arc::ptr_eq(old, new) {
        return 1; // this whole subtree is shared
    }
    if old.kind != new.kind || old.children.len() != new.children.len() {
        return 0;
    }
    count_shared_children(&old.children, &new.children)
}

/// One query hit: a token span plus its kind (the parallel scan's unit
/// of work / merge payload).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// The token kind that matched.
    pub kind: u16,
    /// The token span.
    pub span: Range<u32>,
}

/// A chunk of the token stream: `(start index, end index)` bounds into
/// the token list plus the byte range it covers — the `SoA` chunk-plan
/// shape (`structural_index::ChunkPlan` mirrored here dependency-free).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// First token index (inclusive).
    pub first: usize,
    /// End token index (exclusive).
    pub last: usize,
    /// The byte range covered.
    pub span: Range<u32>,
}

/// Split a token list into `target` contiguous chunks (deterministic
/// even splits — the parallel query's planning step).
#[must_use]
pub fn plan_chunks(tokens: &[(u16, Range<u32>)], target: usize) -> Vec<Chunk> {
    let total = tokens.len();
    if total == 0 {
        return Vec::new();
    }
    let target = target.max(1).min(total);
    let per = total / target;
    let mut chunks = Vec::with_capacity(target);
    let mut first = 0_usize;
    for i in 0..target {
        let last = if i + 1 == target { total } else { first + per };
        if first >= last {
            break;
        }
        let span = tokens[first].1.start..tokens[last - 1].1.end;
        chunks.push(Chunk { first, last, span });
        first = last;
    }
    chunks
}

/// Resolve the effective worker count (the structural index's
/// region-parallel contract): `0` = auto
/// ([`std::thread::available_parallelism`]), any count capped at the
/// chunk count (never more workers than work), and zero chunks means
/// zero workers (no threads are spawned at all).
fn worker_count(workers: usize, chunks: usize) -> usize {
    if chunks == 0 {
        return 0;
    }
    let requested = if workers == 0 {
        std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
    } else {
        workers
    };
    requested.max(1).min(chunks)
}

/// Scan one chunk for tokens passing `scan` (the per-worker step — a
/// pure function over the `SoA` token arrays).
fn scan_chunk<F>(chunk: &Chunk, tokens: &[(u16, Range<u32>)], scan: &F) -> Vec<Hit>
where
    F: Fn(u16) -> bool,
{
    tokens[chunk.first..chunk.last]
        .iter()
        .filter(|(kind, _)| scan(*kind))
        .map(|(kind, span)| Hit {
            kind: *kind,
            span: span.clone(),
        })
        .collect()
}

/// Merge chunk results in ascending start-offset order — the
/// deterministic ordered merge. With the chunks planned in ascending
/// order and each chunk's hits already ascending, the merge is a
/// stable k-way concatenation sorted by start.
fn merge(mut hits: Vec<Hit>) -> Vec<Hit> {
    hits.sort_by_key(|hit| hit.span.start);
    hits
}

/// The chunked query mode (issue 036's "parallel query mode over the
/// `SoA` chunk plan with deterministic ordered merge"): plan chunks, scan
/// each on scoped worker threads, and merge in ascending start-offset
/// order. The result MUST equal the sequential scan — the tests pin
/// that byte-for-byte on seeded corpora at every fan-out; the workers
/// own contiguous ascending chunk ranges, so the merge joins per-worker
/// lists in scan order and does not care about the completion order.
///
/// `workers = 0` resolves to [`std::thread::available_parallelism`];
/// any count caps at the chunk count.
#[must_use]
pub fn parallel_query(tokens: &[(u16, Range<u32>)], wanted: u16, workers: usize) -> Vec<Hit> {
    parallel_query_with(tokens, move |kind| kind == wanted, workers)
}

/// The chunked query mode with a caller-provided scan predicate (the
/// general form of [`parallel_query`]'s single-kind scan): every token
/// kind for which `scan` returns `true` becomes a [`Hit`], merged in
/// ascending start-offset order — deterministically, at any fan-out.
///
/// `workers = 0` resolves to [`std::thread::available_parallelism`];
/// any count caps at the chunk count (never more workers than work).
pub fn parallel_query_with<F>(tokens: &[(u16, Range<u32>)], scan: F, workers: usize) -> Vec<Hit>
where
    F: Fn(u16) -> bool + Sync,
{
    let chunks = plan_chunks(tokens, workers);
    let count = worker_count(workers, chunks.len());
    let mut hits = Vec::new();
    if count == 0 {
        return merge(hits);
    }
    // Contiguous ascending chunk ranges: worker `w` owns chunks
    // `[first..last)` — the merge below concatenates the per-worker hit
    // lists in worker order, which is scan order by construction (no
    // completion-order dependence; the final sort keeps the contract
    // explicit). The workers share the predicate by reference (`&F` is
    // `Send` for `F: Sync`).
    let scan = &scan;
    std::thread::scope(|scope| {
        let per = chunks.len() / count;
        let mut first = 0;
        let mut handles = Vec::with_capacity(count);
        for w in 0..count {
            let last = if w + 1 == count {
                chunks.len()
            } else {
                first + per
            };
            let range = &chunks[first..last];
            handles.push(scope.spawn(move || {
                range
                    .iter()
                    .map(|chunk| scan_chunk(chunk, tokens, scan))
                    .collect::<Vec<Vec<Hit>>>()
            }));
            first = last;
        }
        for handle in handles {
            for chunk_hits in handle.join().expect("query worker never panics") {
                hits.extend(chunk_hits);
            }
        }
    });
    merge(hits)
}

/// The sequential reference: a straight scan.
#[must_use]
pub fn sequential_query(tokens: &[(u16, Range<u32>)], wanted: u16) -> Vec<Hit> {
    tokens
        .iter()
        .filter(|(kind, _)| *kind == wanted)
        .map(|(kind, span)| Hit {
            kind: *kind,
            span: span.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        Arc, Chunk, Hit, RedHandle, Version, parallel_query, plan_chunks, sequential_query,
    };
    use crate::tree::{GreenChild, GreenNode};
    use std::ops::Range;

    fn tokens() -> Vec<(u16, Range<u32>)> {
        // kinds cycle 1..4 over 100 tokens
        (0..100)
            .map(|i| {
                let kind = u16::try_from((i % 4) + 1).unwrap_or(1);
                let start = u32::try_from(i * 3).unwrap_or(u32::MAX);
                (kind, start..start + 2)
            })
            .collect()
    }

    /// Run a single-threaded test body through loom's scheduler under
    /// the model-checking feature (loom's `Arc` panics outside a
    /// model; `loom::model` replays the `Fn` body per schedule);
    /// plain execution in normal builds.
    #[cfg(not(feature = "loom"))]
    fn run_model(body: impl Fn()) {
        body();
    }
    #[cfg(feature = "loom")]
    fn run_model(body: impl Fn() + Sync + Send + 'static) {
        loom::model(body);
    }

    #[test]
    fn publish_assigns_versions_and_retains() {
        run_model(|| {
            let mut handle = RedHandle::new();
            assert_eq!(handle.version(), 0);
            assert!(handle.current().is_none());
            // Version 1.
            let root1 = Arc::new(GreenNode {
                kind: 0,
                text_len: 4,
                has_error: false,
                children: Vec::new(),
            });
            assert_eq!(handle.publish(root1.clone()), 0);
            assert_eq!(handle.version(), 1);
            // Version 2.
            let root2 = Arc::new(GreenNode {
                kind: 0,
                text_len: 6,
                has_error: false,
                children: Vec::new(),
            });
            assert_eq!(handle.publish(root2), 0);
            assert_eq!(handle.version(), 2);
            assert_eq!(handle.history().len(), 2, "both versions retained");
            let history: Vec<Version> = handle.history().to_vec();
            assert_eq!(history[0].number, 1);
            let last = handle.history().last().expect("history");
            assert!(Arc::ptr_eq(handle.current().expect("current"), &last.root));
        });
    }

    #[test]
    fn sharing_counts_identical_subtrees() {
        run_model(|| {
            // The shared leaf lives INSIDE the trees — `GreenChild`
            // stores the kernel's `std` `Arc` in every build (loom or
            // not).
            let leaf = || -> std::sync::Arc<GreenNode> {
                std::sync::Arc::new(GreenNode {
                    kind: 1,
                    text_len: 2,
                    has_error: false,
                    children: Vec::new(),
                })
            };
            // Old and new share the SAME leaf Arc.
            let shared_leaf = leaf();
            let old = Arc::new(GreenNode {
                kind: 0,
                text_len: 4,
                has_error: false,
                children: vec![
                    GreenChild::Node(shared_leaf.clone()),
                    GreenChild::Token {
                        kind: 2,
                        range: 2..4,
                    },
                ],
            });
            let new = Arc::new(GreenNode {
                kind: 0,
                text_len: 6,
                has_error: false,
                children: vec![
                    GreenChild::Node(shared_leaf.clone()),
                    GreenChild::Token {
                        kind: 3,
                        range: 4..6,
                    },
                ],
            });
            assert_eq!(super::count_shared(&old, &new), 1, "the leaf is shared");
            let mut handle = RedHandle::new();
            handle.publish(old);
            assert_eq!(handle.publish(new), 1, "publish reports 1 shared subtree");
        });
    }

    #[test]
    fn chunk_plan_is_contiguous_and_deterministic() {
        let tokens = tokens();
        let chunks = plan_chunks(&tokens, 4);
        assert_eq!(chunks.len(), 4);
        assert_eq!(chunks[0].first, 0);
        assert_eq!(chunks.last().map(|c: &Chunk| c.last), Some(100));
        for pair in chunks.windows(2) {
            assert_eq!(pair[0].last, pair[1].first, "contiguous");
        }
        let again = plan_chunks(&tokens, 4);
        assert_eq!(chunks, again);
    }

    #[test]
    fn parallel_merge_equals_sequential_scan() {
        let tokens = tokens();
        let sequential = sequential_query(&tokens, 2);
        for workers in [1, 2, 4, 8] {
            let parallel = parallel_query(&tokens, 2, workers);
            assert_eq!(
                parallel, sequential,
                "workers {workers}: merge order deterministic"
            );
        }
        // The hits themselves are the right tokens.
        assert_eq!(sequential.len(), 25);
        assert!(
            sequential
                .windows(2)
                .all(|pair| pair[0].span.start < pair[1].span.start)
        );
    }

    #[test]
    fn hit_type_layout() {
        let hit = Hit {
            kind: 3,
            span: 4..9,
        };
        assert_eq!(hit.kind, 3);
        assert_eq!(hit.span, 4..9);
    }
}
