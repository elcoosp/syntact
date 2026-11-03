//! Allocation budget gates (issues 018 + 024): dhat-counted tests over
//! the sink modes. Machine-independent (counters, never wall-clock), so
//! they run in CI.
//!
//! Gates:
//! - `validate`: zero allocations on a prebuilt event stream, any size.
//! - `scan` / `skeleton`: <= 2 allocations per KiB on the steady-state
//!   fixture (>= 32 KiB); absolute <= 16 allocations on small/medium.

use std::sync::{Mutex, MutexGuard, PoisonError};

use bench::{
    SCAN_ALLOC_PER_KIB, SKELETON_ALLOC_PER_KIB, SMALL_DOC_MAX_ALLOCS, STEADY_DOC_MIN_BYTES,
    VALIDATE_ALLOC_PER_KIB, kib, medium_document, small_document, steady_state_document,
};
use packs_json::reference;
use runtime_core::sink::{SkeletonSink, TokenSink, ValidationSink, drive_sink};

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

/// The parsed event stream + ranges of a document (allocated OUTSIDE the
/// measured regions).
fn stream_of(source: &str) -> (runtime_core::events::Output, Vec<std::ops::Range<u32>>) {
    let (events, ranges, _errors) = reference::parse_events(source);
    let mut wrapped = Vec::with_capacity(events.len() + 2);
    wrapped.push(runtime_core::events::Event::Start {
        kind: packs_json::reference::ROOT_KIND,
        forward_parent: None,
    });
    wrapped.extend(events.iter().copied());
    wrapped.push(runtime_core::events::Event::Finish);
    let output = runtime_core::events::process(&wrapped).expect("valid stream");
    let plain: Vec<std::ops::Range<u32>> = ranges.into_iter().map(|(_, range)| range).collect();
    (output, plain)
}

#[test]
fn validate_mode_is_allocation_free_at_full_size() {
    let _guard = gate();
    let source = steady_state_document();
    assert!(source.len() >= STEADY_DOC_MIN_BYTES);
    let (output, ranges) = stream_of(&source);
    let (_summary, allocs) = measured(|| {
        let mut sink = ValidationSink::default();
        drive_sink(&output, &ranges, &mut sink).expect("drives");
        sink.summary()
    });
    let budget = VALIDATE_ALLOC_PER_KIB as u64 * kib(source.len()) as u64;
    assert_eq!(
        allocs, 0,
        "validate mode allocated {allocs} times (budget {budget}) on a prebuilt stream"
    );
}

#[test]
fn scan_mode_stays_under_per_kib_budget() {
    let _guard = gate();
    let source = steady_state_document();
    let (output, ranges) = stream_of(&source);
    let (_tokens, allocs) = measured(|| {
        let mut sink = TokenSink::default();
        drive_sink(&output, &ranges, &mut sink).expect("drives");
        sink.tokens.len()
    });
    let budget = SCAN_ALLOC_PER_KIB as u64 * kib(source.len()) as u64;
    assert!(
        allocs <= budget,
        "scan mode used {allocs} allocs, budget {budget} ({} KiB)",
        kib(source.len())
    );
}

#[test]
fn skeleton_mode_stays_under_per_kib_budget() {
    let _guard = gate();
    let source = steady_state_document();
    let (output, ranges) = stream_of(&source);
    let (_nodes, allocs) = measured(|| {
        let mut sink = SkeletonSink::default();
        drive_sink(&output, &ranges, &mut sink).expect("drives");
        sink.nodes.len()
    });
    let budget = SKELETON_ALLOC_PER_KIB as u64 * kib(source.len()) as u64;
    assert!(
        allocs <= budget,
        "skeleton mode used {allocs} allocs, budget {budget} ({} KiB)",
        kib(source.len())
    );
}

#[test]
fn small_and_medium_fixtures_pin_absolute_allocs() {
    let _guard = gate();
    for source in [small_document(), medium_document()] {
        let (output, ranges) = stream_of(&source);
        let scan_allocs = measured(|| {
            let mut sink = TokenSink::default();
            drive_sink(&output, &ranges, &mut sink).expect("drives");
        })
        .1;
        assert!(
            scan_allocs <= SMALL_DOC_MAX_ALLOCS as u64,
            "scan on a {} B document allocated {scan_allocs} times (cap {SMALL_DOC_MAX_ALLOCS})",
            source.len()
        );
        let skeleton_allocs = measured(|| {
            let mut sink = SkeletonSink::default();
            drive_sink(&output, &ranges, &mut sink).expect("drives");
        })
        .1;
        assert!(
            skeleton_allocs <= SMALL_DOC_MAX_ALLOCS as u64,
            "skeleton on a {} B document allocated {skeleton_allocs} times (cap {SMALL_DOC_MAX_ALLOCS})",
            source.len()
        );
    }
}

#[test]
fn mode_matrix_parity_holds() {
    let _guard = gate();
    // validate == full (shape counts), scan == full (token counts) — the
    // sink × engine matrix must agree on the same stream.
    let source = medium_document();
    let (output, ranges) = stream_of(&source);
    let mut validation = ValidationSink::default();
    drive_sink(&output, &ranges, &mut validation).expect("drives");
    let summary = validation.summary();
    assert!(summary.balanced);
    // The stream's own shape is the oracle: Enter steps == nodes seen,
    // token ranges == tokens consumed.
    let enters = output
        .steps()
        .filter(|step| matches!(step, runtime_core::events::Step::Enter { .. }))
        .count();
    assert_eq!(summary.nodes as usize, enters);
    assert_eq!(summary.tokens as usize, ranges.len());
    let mut tokens = TokenSink::default();
    drive_sink(&output, &ranges, &mut tokens).expect("drives");
    assert_eq!(tokens.tokens.len(), ranges.len());
}

#[test]
fn fixture_is_byte_faithful_through_the_full_pipeline() {
    let _guard = gate();
    // The steady-state fixture survives parse → render byte-identically
    // (the losslessness gate on the perf fixture itself).
    let source = steady_state_document();
    let tree = reference::parse(&source).expect("parses");
    let input = runtime_core::input::Source::from_str_unchecked(&source);
    assert_eq!(tree.render(&input), source);
}
