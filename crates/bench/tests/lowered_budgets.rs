//! Generated-path allocation budget gates (issue 043): dhat-counted
//! tests over the descriptor-compiler pipeline, machine-independent
//! (counters, never wall-clock), so they run in CI.
//!
//! Gates (the ~32 KiB steady JSON fixture):
//! - `lower` (grammar → descriptors): a one-time compile step, pinned
//!   absolutely — <= 600 blocks regardless of document size (the
//!   grammar, not the document, drives it).
//! - generated `green_tree` parse: <= 5× the reference pack's blocks
//!   per KiB on the same fixture (measured 4.5×) — the interpreter
//!   allocates the same event/tree shapes plus the engine's per-entry
//!   `RuleDesc` body/sync clones (`descent::Engine::rule` clones the
//!   pattern to satisfy the interpreter's borrows). That clone tax is
//!   the known follow-up (share-once descriptors); the factor gate
//!   catches runaway regressions meanwhile.
//! - generated `feed`: the scan is a flat per-lexeme Vec — caught by
//!   the same factor gate through `green_tree` (it dominates).

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

/// The lowered fixture pipeline: descriptors + scanner + kind maps.
fn pipeline() -> (descent_lower::Lowered, lexgen::Scanner) {
    let grammar = load_grammar(descent_lower::JSON_GRAMMAR).expect("fixture loads");
    let lowered = lower(&grammar).expect("fixture lowers");
    let scanner = lexgen::compile(&grammar).expect("fixture lexes");
    (lowered, scanner)
}

/// The absolute compile-step gate: lowering is grammar-proportional,
/// never document-proportional.
const LOWER_MAX_BLOCKS: u64 = 600;

#[test]
fn lower_compile_budget() {
    let _guard = gate();
    let (grammar, blocks, _bytes) =
        measured(|| load_grammar(descent_lower::JSON_GRAMMAR).expect("loads"));
    let (lowered, lower_blocks, _lower_bytes) = measured(|| lower(&grammar).expect("lowers"));
    assert_eq!(lowered.desc.rules.len(), 3, "three node rules");
    assert!(
        lower_blocks <= LOWER_MAX_BLOCKS,
        "lower() allocated {lower_blocks} blocks (gate {LOWER_MAX_BLOCKS}); \
         the compile step must stay grammar-proportional"
    );
    // The load half (KDL → IR) is pinned as context, not a gate — it is
    // gir-syntax's budget, but a jump there shows up here too.
    assert!(blocks > 0, "load allocates");
}

/// The generated parse path stays within a constant factor of the
/// hand-written reference on the same fixture (measured headroom above
/// the engine's per-entry descriptor clones).
const GENERATED_VS_REFERENCE_FACTOR: u64 = 5;

#[test]
fn generated_parse_budget_per_kib() {
    let _guard = gate();
    let source = steady_document();
    let (lowered, scanner) = pipeline();
    let trivia = lowered.trivia_kind.expect("fixture has trivia");

    let (generated_tree, generated_blocks, _bytes) = measured(|| {
        let tokens = lowered.feed(&scanner, &source);
        let run = descent::parse_events(&lowered.desc, &tokens, &source);
        descent::green_tree(&run, &source, packs_json::reference::ROOT_KIND, trivia)
            .expect("tree integration")
    });
    let (_reference_tree, reference_blocks, _ref_bytes) =
        measured(|| packs_json::parse(&source).expect("reference parses"));

    assert_eq!(
        generated_tree.render(&runtime_core::input::Source::from_str_unchecked(&source)),
        source,
        "byte-faithful before counting"
    );
    assert!(
        generated_blocks <= GENERATED_VS_REFERENCE_FACTOR * reference_blocks,
        "generated path allocated {generated_blocks} blocks vs reference \
         {reference_blocks} (factor gate {GENERATED_VS_REFERENCE_FACTOR}x)"
    );
}
