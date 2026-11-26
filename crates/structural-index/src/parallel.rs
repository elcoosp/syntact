//! The region-parallel chunk drive: execute a balanced chunk plan on
//! scoped worker threads — the execution half of the region-parallel
//! roadmap item (§7.9 "parallel chunk parsing", §12.2 "region-parallel
//! scanning"), the shape the chunk-validation seam
//! ([`crate::chunks`]) documents itself as running on.
//!
//! # Design (the invariants the tests pin)
//!
//! - **Isolation**: every chunk is a balanced, independently
//!   parseable byte range ([`crate::plan::plan_chunks`] guarantees the
//!   boundaries), so a worker validates its chunks with no shared
//!   state and no cross-chunk ordering constraints.
//! - **Deterministic ordered merge**: chunks are planned in scan
//!   order, workers own contiguous ascending chunk ranges, and the
//!   per-worker fault lists are joined in worker order — the merged
//!   [`ChunkReport`] is in scan order regardless of thread completion
//!   order, so the parallel verdict IS the sequential verdict
//!   ([`crate::chunks::validate_chunks`]) byte-for-byte. (The same
//!   determinism contract `runtime-core::persistent::parallel_query`
//!   documents for the query analogue.)
//! - **Fresh validator per chunk**: the generic drive takes a factory
//!   and builds one [`ChunkValidator`] per chunk — a stateful
//!   validator never sees another chunk's bytes, and the built-in
//!   [`ChunkFaultScanner`] is identical through either path.
//! - **Bounded fan-out**: `workers = 0` resolves to
//!   [`std::thread::available_parallelism`]; any count caps at the
//!   chunk count (never more workers than work).
//!
//! No `unsafe` anywhere: `std::thread::scope` bounds every worker's
//! borrows of the source and the plan.

use std::thread;

use crate::chunks::{
    ChunkFault, ChunkFaultScanner, ChunkReport, ChunkValidator, chunk_text, rebase,
};
use crate::index::RegionIndex;
use crate::plan::{ChunkPlan, plan_chunks};

/// Resolve the effective worker count: `0` = auto
/// ([`std::thread::available_parallelism`]), any count capped at the
/// chunk count (never more workers than work), and zero chunks means
/// zero workers (no threads are spawned at all).
fn worker_count(workers: usize, chunks: usize) -> usize {
    if chunks == 0 {
        return 0;
    }
    let requested = if workers == 0 {
        thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
    } else {
        workers
    };
    requested.max(1).min(chunks)
}

/// Validate every chunk of `plan` on scoped worker threads with the
/// built-in [`ChunkFaultScanner`], merging the verdicts into the same
/// attributed [`ChunkReport`] the sequential
/// [`validate_chunks`](crate::chunks::validate_chunks) returns —
/// parity pinned by the tests.
///
/// `workers = 0` resolves to [`std::thread::available_parallelism`];
/// any count caps at the chunk count. The drive runs every chunk (no
/// early exit), so the report carries ALL faults in scan order.
#[must_use]
pub fn validate_chunks_parallel(source: &str, plan: &[ChunkPlan], workers: usize) -> ChunkReport {
    validate_chunks_parallel_with(source, plan, workers, || ChunkFaultScanner)
}

/// Validate every chunk of `plan` on scoped worker threads, building
/// one fresh `V` per chunk through `factory` — a stateful validator
/// never sees another chunk's bytes (the built-in scanner path is
/// [`validate_chunks_parallel`]). The merged report is the sequential
/// [`validate_chunks`](crate::chunks::validate_chunks) verdict for any
/// stateless validator, fault attribution included.
///
/// `workers = 0` resolves to [`std::thread::available_parallelism`];
/// any count caps at the chunk count.
pub fn validate_chunks_parallel_with<F, V>(
    source: &str,
    plan: &[ChunkPlan],
    workers: usize,
    factory: F,
) -> ChunkReport
where
    F: Fn() -> V + Sync,
    V: ChunkValidator,
{
    let count = worker_count(workers, plan.len());
    let mut faults = Vec::new();
    if count == 0 {
        return ChunkReport {
            chunks: plan.len(),
            faults,
        };
    }
    // Contiguous ascending chunk ranges: worker `w` owns chunks
    // `[first..last)` — the merge below concatenates the per-worker
    // fault lists in worker order, which is scan order by
    // construction (no sort, no completion-order dependence).
    let per = plan.len() / count;
    let mut first = 0;
    // The workers share the factory by reference (`&F` is `Send` for
    // `F: Sync`); the `move` closures copy that reference in.
    let factory = &factory;
    thread::scope(|scope| {
        let mut handles = Vec::with_capacity(count);
        for w in 0..count {
            let last = if w + 1 == count {
                plan.len()
            } else {
                first + per
            };
            let range = &plan[first..last];
            let base = first;
            // `move` copies the captures in: `range`/`source`/`factory`
            // are shared references (Copy) and `base` is a `usize`, so
            // each worker owns its own copies and `first`/`plan`
            // stay usable for the next iteration.
            handles.push(scope.spawn(move || {
                let mut worker_faults = Vec::new();
                for (offset, chunk) in range.iter().enumerate() {
                    let idx = base + offset;
                    if let Err(fault) = factory().validate_chunk(chunk_text(source, chunk)) {
                        worker_faults.push(ChunkFault {
                            chunk: idx,
                            fault: rebase(fault, chunk.start),
                        });
                    }
                }
                worker_faults
            }));
            first = last;
        }
        // Join in worker order: the merged fault list is scan order
        // regardless of which thread finishes first.
        for handle in handles {
            faults.extend(handle.join().expect("chunk worker never panics"));
        }
    });
    ChunkReport {
        chunks: plan.len(),
        faults,
    }
}

/// Plan and validate in one call on scoped worker threads:
/// [`plan_chunks`] cuts `target` balanced chunks out of the index,
/// then [`validate_chunks_parallel`] drives them — the parallel form
/// of [`validate_plan`](crate::chunks::validate_plan).
#[must_use]
pub fn validate_plan_parallel(
    source: &str,
    index: &RegionIndex,
    target: usize,
    workers: usize,
) -> ChunkReport {
    validate_chunks_parallel(source, &plan_chunks(source, index, target), workers)
}
