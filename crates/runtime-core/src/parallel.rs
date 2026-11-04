//! # parallel — deterministic parallel query mode (issue 036, §7.9)
//!
//! Parallel query execution over a **chunk plan** (contiguous,
//! bracket-balanced byte ranges — the `SoA` region index's
//! `plan_chunks` output):
//!
//! - workers run per-chunk queries via [`std::thread::scope`] (no
//!   unsafe, no rayon);
//! - **the merge is ordered by chunk position, not completion** —
//!   chunk `i`'s result lands at output index `i`, so the result is
//!   byte-for-byte deterministic regardless of scheduling;
//! - single-writer / multi-reader friendly: the input tables are
//!   shared immutably (`&`), results are owned per worker and moved
//!   home.

use std::ops::Range;

use crate::persistent::ChunkPlan;

/// Run `f` over every chunk on the worker pool; the merge is ordered
/// by chunk position (deterministic by construction). One chunk (or
/// none) runs inline — no worker spawn.
///
/// # Panics
/// If a worker panics, the scope join propagates it (fail-fast).
pub fn map_chunks<T, F>(chunks: &ChunkPlan, f: F) -> Vec<T>
where
    F: Fn(&Range<u32>) -> T + Send + Sync,
    T: Send,
{
    if chunks.len() <= 1 {
        return chunks.iter().map(f).collect();
    }
    // Pre-size the result so every worker writes its own slot — the
    // ordered merge is an index write, never a completion race.
    let results: Vec<std::sync::Mutex<Option<T>>> =
        chunks.iter().map(|_| std::sync::Mutex::new(None)).collect();
    let shared = &f; // workers share the closure by reference
    std::thread::scope(|scope| {
        for (idx, chunk) in chunks.iter().enumerate() {
            let slot = &results[idx];
            scope.spawn(move || {
                let value = shared(chunk);
                *slot.lock().expect("worker slot lock") = Some(value);
            });
        }
    });
    results
        .into_iter()
        .map(|slot| match slot.into_inner() {
            Ok(Some(value)) => value,
            _ => unreachable!("every worker wrote its slot"),
        })
        .collect()
}

/// Per-chunk hit counts over a `SoA` `(kinds, ranges)` token/region
/// table — the parallel query mode's reference consumer. The output
/// index follows chunk order; hits whose range center falls in the
/// chunk count toward it.
#[must_use]
pub fn hits_per_chunk(
    chunks: &ChunkPlan,
    kinds: &[u16],
    ranges: &[Range<u32>],
    want: u16,
) -> Vec<usize> {
    let rows: Vec<(u16, &Range<u32>)> = kinds.iter().copied().zip(ranges.iter()).collect();
    map_chunks(chunks, move |chunk| {
        rows.iter()
            .filter(|(_, r)| {
                let center = r.start + (r.end - r.start) / 2;
                chunk.start <= center && center < chunk.end
            })
            .filter(|(kind, _)| *kind == want)
            .count()
    })
}

/// Per-chunk byte coverage (sum of range lengths whose center falls
/// in the chunk) — the merge-order probe used by the determinism test.
#[must_use]
pub fn bytes_per_chunk(chunks: &ChunkPlan, ranges: &[Range<u32>]) -> Vec<u32> {
    map_chunks(chunks, move |chunk| {
        ranges
            .iter()
            .filter(|r| {
                let center = r.start + (r.end - r.start) / 2;
                chunk.start <= center && center < chunk.end
            })
            .map(|r| r.end.saturating_sub(r.start))
            .sum()
    })
}
