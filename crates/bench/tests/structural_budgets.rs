//! Structural-index allocation gates: the streaming validate fast path
//! must allocate ZERO heap blocks — the "without building the index"
//! contract of `validate_fast` (issue 032, ADR-011), enforced the same
//! way the sink-mode budgets are.

use std::sync::{Mutex, MutexGuard, PoisonError};

use bench::{STEADY_DOC_MIN_BYTES, steady_state_document};
use structural_index::validate_fast;

/// dhat global allocator (allocation counting).
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// dhat counters are process-global: budget tests serialize so parallel
/// fixtures never pollute a measured window.
static GATE: Mutex<()> = Mutex::new(());

/// Hold this for the whole body of a budget test (fixture building
/// included).
fn gate() -> MutexGuard<'static, ()> {
    GATE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Run `f` under a heap profiler; returns the output plus the number of
/// allocations made inside `f`.
fn measured<T>(f: impl FnOnce() -> T) -> (T, u64) {
    let profiler = dhat::Profiler::new_heap();
    let before = dhat::HeapStats::get();
    let out = f();
    let after = dhat::HeapStats::get();
    drop(profiler);
    (out, after.total_blocks - before.total_blocks)
}

#[test]
fn validate_fast_allocates_zero_blocks_on_the_steady_document() {
    let _guard = gate();
    let source = steady_state_document();
    assert!(
        source.len() >= STEADY_DOC_MIN_BYTES,
        "the steady fixture is the budgeted size"
    );
    let (verdict, blocks) = measured(|| validate_fast(&source));
    assert_eq!(verdict, Ok(()), "the steady fixture is balanced");
    assert_eq!(blocks, 0, "streaming validate must not allocate");
}
