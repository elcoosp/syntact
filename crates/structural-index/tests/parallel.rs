//! The region-parallel chunk drive: execute a balanced [`ChunkPlan`]
//! on scoped worker threads and merge the per-chunk verdicts into the
//! same attributed [`ChunkReport`] the sequential drive returns.
//!
//! This is the plan-listed region-parallel scanning seam: the roadmap's
//! structural-index section (`docs/architecture-and-roadmap.md` §7.9)
//! names "parallel chunk parsing for grammars with declared independent
//! regions" as the indexer's use, §12.2 lists "region-parallel
//! scanning" first among the unshipped P4 work, and the P4 exit gate
//! (§12.3) measures "region-parallel ≥1.5× on ≥2 packs" on top of it.
//! The correctness precondition every one of those gates runs on is
//! pinned here: the parallel verdict IS the sequential verdict — same
//! `chunks` count, same attributed faults with the same rebased global
//! offsets — for every worker fan-out, and the merged fault order is
//! scan order regardless of thread completion order.

use structural_index::{
    ChunkFault, ChunkPlan, ChunkReport, ChunkValidator, StructuralFault, plan_chunks, scan,
    validate_chunks, validate_chunks_parallel, validate_chunks_parallel_with, validate_fast,
    validate_plan, validate_plan_parallel,
};

/// Deterministic xorshift64* (same generator as the fuzz suite).
struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            0
        } else {
            let b = u64::try_from(bound).unwrap_or(u64::MAX);
            usize::try_from(self.next() % b).unwrap_or(0)
        }
    }
}

/// Clean sources with several true depth-0 code boundaries — targets 2
/// through 4 all cut the source into genuinely balanced chunks (the
/// precondition `plan_chunks` guarantees and `chunks.rs` tests pin).
/// The top-level strings and line/block comments matter: a string or
/// comment closing at depth 0 is exactly what makes the Code region
/// after it a chunk-boundary candidate.
const CLEAN_SOURCES: [&str; 4] = [
    "fn main() { let x = \"}\"; }\nlet flat = \"top\";\nfn second() { let y = \"(\"; }\n// note\nfn third() { }\n",
    "{\"k\": [1, 2, {\"inner\": true}], \"k2\": \"}\"}\n// between objects\n{\"k3\": null}\nlet flat = \"flat\";\n{\"k4\": []}\n",
    "let a = 1; { let b = 2; } let c = 3;\n// note\nfn f() { /* } { */ }\n/* flat */\nfn g() { }\n\"top\";\nfn h() { }\n",
    "x = \"unterminated-looking \\\" fine\";\nif ok { run(a, b); }\nelse { /* ' */ }\n// }} strays in a comment\nlet flat = \"flat\";\n",
];

/// The same sources with one chunk's balance deliberately broken: an
/// unterminated string, a stray close, an unterminated comment, and an
/// unclosed brace.
const FAULTED_SOURCES: [&str; 4] = [
    "fn main() { let x = \"}\"; }\nlet flat = \"top\";\nfn second() { let y = \"(unterminated\n",
    "{\"k\": [1, 2, {\"inner\": true}], \"k2\": \"}\"}\n// between objects\n{\"k3\": null}\n} stray\n",
    "let a = 1; { let b = 2; } let c = 3;\n// note\nfn f() { /* unterminated }\n",
    "x = \"closed\";\nif ok { run(a, b);\n",
];

/// All worker fan-outs the drive documents: 0 = auto
/// (`available_parallelism`), plus explicit 1/2/4/8 and a fan-out
/// wider than the plan (workers > chunks).
const WORKER_FANOUTS: [usize; 5] = [0, 1, 2, 4, 8];

/// The fan-out must actually engage: at target 3 the clean sources cut
/// MORE than one chunk (their top-level strings and comments close at
/// depth 0, making interior boundaries real). Guards the parity gates
/// below against silently degrading to single-chunk plans.
#[test]
fn the_sources_cut_multi_chunk_plans() {
    for source in CLEAN_SOURCES {
        let index = scan(source);
        let plan = plan_chunks(source, &index, 3);
        assert!(plan.len() >= 2, "target 3 on {source:?} cut {plan:?}");
    }
}

/// THE plan-fidelity gate: the parallel drive IS the sequential drive.
/// Roadmap §7.9 (the acceptance criterion this test encodes): the
/// structural index feeds "parallel chunk parsing for grammars with
/// declared independent regions" — decomposition is only sound if the
/// chunked-parallel verdict equals the whole-source chunked verdict.
#[test]
fn parallel_drive_matches_the_sequential_verdict() {
    for source in CLEAN_SOURCES.into_iter().chain(FAULTED_SOURCES) {
        let index = scan(source);
        for target in [1, 2, 3, 4] {
            let plan = plan_chunks(source, &index, target);
            let sequential = validate_chunks(source, &plan);
            for workers in WORKER_FANOUTS {
                let parallel = validate_chunks_parallel(source, &plan, workers);
                assert_eq!(
                    parallel, sequential,
                    "workers {workers}, target {target} diverged on {source:?}"
                );
            }
        }
    }
}

/// Fault attribution survives the fan-out: the chunk index, the fault
/// kind, and the rebased global offsets all match the sequential
/// report's `first_fault` — the shape downstream attribution reads.
#[test]
fn fault_attribution_matches_the_sequential_report() {
    for source in FAULTED_SOURCES {
        let index = scan(source);
        let plan = plan_chunks(source, &index, 3);
        let sequential = validate_chunks(source, &plan);
        let parallel = validate_chunks_parallel(source, &plan, 4);
        assert_eq!(parallel.first_fault(), sequential.first_fault());
        assert_eq!(parallel.chunks, sequential.chunks);
        let expect: Option<ChunkFault> = sequential.faults.first().copied();
        assert_eq!(parallel.first_fault(), expect);
        // The rebased offset points at the real damage in source
        // coordinates (sanity against a rebasing regression).
        if let Some(ChunkFault {
            fault: StructuralFault::StrayClose(at),
            ..
        }) = parallel.first_fault()
        {
            assert_eq!(
                &source[at as usize..=at as usize],
                "}",
                "StrayClose offset must land on the brace itself"
            );
        }
    }
}

/// Determinism: the same plan driven twice with the same fan-out
/// yields byte-identical reports — the merged fault order is scan
/// order, not thread completion order.
#[test]
fn the_drive_is_deterministic_under_repetition() {
    let source = FAULTED_SOURCES[1];
    let index = scan(source);
    let plan = plan_chunks(source, &index, 4);
    let first = validate_chunks_parallel(source, &plan, 8);
    let again = validate_chunks_parallel(source, &plan, 8);
    assert_eq!(first, again);
}

/// Degenerate plans never panic and never spawn: an empty plan is a
/// clean report with zero chunks; a fan-out wider than the plan caps
/// at one worker per chunk; a single chunk works at every fan-out.
#[test]
fn degenerate_plans_are_clean_and_panic_free() {
    let empty: [ChunkPlan; 0] = [];
    let report = validate_chunks_parallel("anything", &empty, 4);
    assert_eq!(
        report,
        ChunkReport {
            chunks: 0,
            faults: Vec::new()
        }
    );
    assert!(report.is_clean());

    let source = CLEAN_SOURCES[0];
    let index = scan(source);
    let single = plan_chunks(source, &index, 1);
    for workers in WORKER_FANOUTS {
        let report = validate_chunks_parallel(source, &single, workers);
        assert!(report.is_clean(), "workers {workers}");
        assert_eq!(report.chunks, 1);
    }
    // Wide fan-out over a 3-chunk plan caps at 3 and stays in parity.
    let plan = plan_chunks(source, &index, 3);
    let wide = validate_chunks_parallel(source, &plan, 64);
    assert_eq!(wide, validate_chunks(source, &plan));
}

/// The planning convenience in one call: `validate_plan_parallel` ==
/// sequential `validate_plan` at every fan-out.
#[test]
fn plan_and_drive_in_one_call_matches_validate_plan() {
    for source in CLEAN_SOURCES.into_iter().chain(FAULTED_SOURCES) {
        let index = scan(source);
        for target in [2, 4] {
            for workers in WORKER_FANOUTS {
                assert_eq!(
                    validate_plan_parallel(source, &index, target, workers),
                    validate_plan(source, &index, target),
                    "workers {workers}, target {target}"
                );
            }
        }
    }
}

/// The `parallel_with` factory contract: one FRESH validator per chunk
/// (a stateful validator never sees another chunk's bytes), and the
/// built-in scanner parity still holds through the generic path.
#[test]
fn the_factory_builds_one_validator_per_chunk() {
    use std::cell::Cell;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // How many validator instances the factory built (one per chunk —
    // a per-worker factory would build fewer on a multi-chunk plan).
    static INSTANCES: AtomicUsize = AtomicUsize::new(0);

    // A validator that records every chunk IT saw in the shared log.
    struct Counter {
        log: Arc<Mutex<Vec<usize>>>,
        seen: Cell<usize>,
    }
    impl ChunkValidator for Counter {
        fn validate_chunk(&mut self, text: &str) -> Result<(), StructuralFault> {
            self.seen.set(self.seen.get() + 1);
            self.log.lock().expect("log").push(text.len());
            Ok(())
        }
    }

    let source = CLEAN_SOURCES[0];
    let index = scan(source);
    let plan = plan_chunks(source, &index, 4);
    let log = Arc::new(Mutex::new(Vec::new()));
    let report = validate_chunks_parallel_with(source, &plan, 2, || {
        INSTANCES.fetch_add(1, Ordering::SeqCst);
        Counter {
            log: Arc::clone(&log),
            seen: Cell::new(0),
        }
    });
    assert!(report.is_clean());
    assert_eq!(
        INSTANCES.load(Ordering::SeqCst),
        plan.len(),
        "the factory must build exactly one validator per chunk"
    );
    assert_eq!(
        log.lock().expect("log").len(),
        plan.len(),
        "every chunk was validated exactly once"
    );
}

/// Seeded breadth: 64 random documents — clean and with one chunk
/// broken — driven at every fan-out stay in parity with the sequential
/// verdict (the same oracle shape the fuzz suite pins per chunk).
/// Top-level strings and comments are in the mix precisely so plans
/// cut multiple chunks; the engagement floor guards that property.
#[test]
fn seeded_corpus_stays_in_parity_at_every_fanout() {
    let mut rng = Rng::new(0x5EED_C0DE);
    let mut max_chunks = 0;
    for _ in 0..64 {
        let mut source = String::new();
        for _ in 0..24 {
            match rng.below(8) {
                0 => source.push_str("fn item() { let s = \"}\"; }\n"),
                1 => source.push_str("let x = 1; { /* } { */ }\n"),
                2 => source.push_str("{\"k\": [1, {\"in\": true}]}\n"),
                3 => source.push_str("// stray } brace in a comment\n"),
                4 => source.push_str("let s = 'c'; let t = \"(\");\n"),
                5 => source.push_str("let flat = \"flat string\";\n"),
                6 => source.push_str("/* flat block */\n"),
                _ => source.push_str("fn deep() { if a { while b { c(); } } }\n"),
            }
        }
        if rng.below(2) == 1 {
            // Break one chunk's balance mid-document.
            let at = rng.below(source.len());
            source.insert(at, '\n');
            source.push_str("fn broken() { let s = \"open\n");
        }
        let index = scan(&source);
        let sequential_clean = validate_fast(&source);
        for target in [1, 2, 3, 5] {
            let plan = plan_chunks(&source, &index, target);
            max_chunks = max_chunks.max(plan.len());
            let sequential = validate_chunks(&source, &plan);
            for workers in [0, 1, 3, 8] {
                let parallel = validate_chunks_parallel(&source, &plan, workers);
                assert_eq!(
                    parallel, sequential,
                    "workers {workers}, target {target} diverged on {source:?}"
                );
            }
            // And the merged verdict still equals the whole-source
            // fast path: clean exactly when the source is clean.
            assert_eq!(sequential.is_clean(), sequential_clean.is_ok());
        }
    }
    assert!(
        max_chunks >= 4,
        "the corpus stopped producing multi-chunk plans (max {max_chunks})"
    );
}
