//! Generated-path allocation budget gate (issue 045): dhat-counted tests
//! over the generated parser crate (the codegen-rust descent backend's
//! emitted output, regenerated on every build) on the ~32 KiB steady
//! JSON fixture. Machine-independent (counters, never wall-clock), so
//! they run in CI.

use descent_lower::lower;
use gir_syntax::load_grammar;

/// dhat global allocator (allocation counting).
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// dhat counters are process-global: budget tests serialize so parallel
/// fixtures never pollute a measured window.
static GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn gate() -> std::sync::MutexGuard<'static, ()> {
    GATE.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Run `f` under a heap profiler; returns the output plus the number of
/// allocations made inside `f`.
fn measured<T>(f: impl FnOnce() -> T) -> (T, u64, u64) {
    let profiler = dhat::Profiler::new_heap();
    let before = dhat::HeapStats::get();
    let out = f();
    let after = dhat::HeapStats::get();
    drop(profiler);
    (
        out,
        after.total_blocks - before.total_blocks,
        after.total_bytes - before.total_bytes,
    )
}

/// The steady-state JSON fixture (byte-stable, shared with the benches).
fn steady_document() -> String {
    bench::steady_state_document()
}

/// The lowered fixture pipeline (the engine's inputs).
fn engine_pipeline() -> (descent_lower::Lowered, lexgen::Scanner) {
    let grammar = load_grammar(descent_lower::JSON_GRAMMAR).expect("fixture loads");
    let lowered = lower(&grammar).expect("fixture lowers");
    let scanner = lexgen::compile(&grammar).expect("fixture lexes");
    (lowered, scanner)
}

/// The generated parser's allocations must stay within a constant factor
/// of the hand-written reference on the same fixture. The generated parser
/// produces the same event/tree shapes the reference does; the factor
/// gate catches runaway regressions (e.g. a per-event allocation the
/// reference avoids). Measured headroom above the engine's per-entry
/// descriptor clones, which the generated parser eliminates by
/// construction (one `fn` per node rule, no `RuleDesc` body/sync clones).
const GENERATED_VS_REFERENCE_FACTOR: u64 = 2;

#[test]
fn generated_parse_allocations_within_factor_of_reference() {
    let _guard = gate();
    let source = steady_document();
    let (_gen_tree, gen_blocks, _gen_bytes) =
        measured(|| generated_json::parse(&source).expect("generated parses"));
    let (_ref_tree, ref_blocks, _ref_bytes) =
        measured(|| packs_json::parse(&source).expect("reference parses"));
    assert!(
        gen_blocks <= ref_blocks.saturating_mul(GENERATED_VS_REFERENCE_FACTOR),
        "generated parse allocated {gen_blocks} blocks (reference {ref_blocks}, \
         factor gate {GENERATED_VS_REFERENCE_FACTOR}×); the generated parser must \
         stay within a constant factor of the reference on the steady fixture"
    );
}

#[test]
fn generated_parse_events_within_factor_of_engine() {
    // Post-M2 (issue 045): the engine's per-entry RuleDesc clones were
    // eliminated, so the engine is now allocation-parity with the reference.
    // The generated parser pays a one-time `OnceLock` scanner-rebuild cost
    // (the serialized DFA tables reconstructed as `Vec`s on first call) that
    // the engine doesn't — amortized across many parses, but visible on a
    // single-call measurement. The gate keeps the generated parser within a
    // constant factor of the engine rather than strictly ≤ (the engine is no
    // longer the allocation ceiling; the reference is, see
    // `generated_parse_allocations_within_factor_of_reference`).
    let _guard = gate();
    let source = steady_document();
    let (_gen_run, gen_blocks, _gen_bytes) = measured(|| generated_json::parse_events(&source));
    let (lowered, scanner) = engine_pipeline();
    let (_eng_run, eng_blocks, _eng_bytes) = measured(|| {
        let tokens = lowered.feed(&scanner, &source);
        descent::parse_events(&lowered.desc, &tokens, &source)
    });
    // The one-time scanner rebuild (OnceLock init) is the gap; allow 4× to
    // keep the gate meaningful without flagging the amortized one-time cost.
    assert!(
        gen_blocks <= eng_blocks.saturating_mul(4),
        "generated parse_events allocated {gen_blocks} blocks (engine {eng_blocks}); \
         the generated parser must stay within a constant factor of the engine"
    );
}
