//! The region-parallel chunk drive as a consumer: plan balanced
//! chunks from the region index, validate them on scoped worker
//! threads through the built-in scanner and a custom checker riding
//! the same parallel seam, and read the merged report — the parallel
//! verdict IS the sequential verdict (parity asserted on every
//! sample), with faults attributed to their chunk at global offsets.
//!
//! Run: `cargo run -p structural-index --example region_parallel`

use std::time::Instant;

use structural_index::{
    ChunkValidator, FaultScanner, StructuralFault, plan_chunks, scan, scan_regions,
    validate_chunks, validate_chunks_parallel, validate_chunks_parallel_with, validate_plan,
    validate_plan_parallel,
};

/// A custom per-chunk checker riding the parallel seam: runs the real
/// fail-fast scan on each chunk (so its verdict matches the built-in
/// scanner's) and can inject one synthetic fault to demonstrate
/// attribution. Instances never share state across threads — the
/// factory builds one fresh checker per chunk.
struct Probe {
    reject: Option<usize>,
    calls: usize,
}

impl ChunkValidator for Probe {
    fn validate_chunk(&mut self, text: &str) -> Result<(), StructuralFault> {
        self.calls += 1;
        if self.reject == Some(self.calls) {
            return Err(StructuralFault::UnclosedBraces(1));
        }
        let mut scanner = FaultScanner::new(text);
        let _ = scan_regions(text, &mut scanner);
        scanner.finish()
    }
}

fn show(source: &str, report: &structural_index::ChunkReport) {
    println!(
        "{source:?}: {} chunks, {}",
        report.chunks,
        if report.is_clean() {
            "clean".to_string()
        } else {
            format!("first fault: {:?}", report.first_fault())
        }
    );
}

fn main() {
    // Top-level strings and comments between braces matter: a string
    // or comment closing at depth 0 makes the Code region after it a
    // chunk-boundary candidate, so target 4 cuts a real multi-chunk
    // plan (single nested objects would plan one chunk).
    let sources = [
        "fn main() { let x = \"}\"; }\nlet flat = \"top\";\nfn second() { let y = \"(\"; }\n// note\nfn third() { }\n",
        "{\"k\": [1, 2, {\"inner\": true}], \"k2\": \"}\"}\n// between objects\n{\"k3\": null}\nlet flat = \"flat\";\n{\"k4\": []}\n",
        "let s = \"unterminated {",
    ];
    for source in sources {
        let index = scan(source);
        let plan = plan_chunks(source, &index, 4);

        // The sequential reference: one validator, one pass.
        let sequential = validate_chunks(source, &plan);
        show(source, &sequential);

        // The parallel drive at several fan-outs: the merged report
        // matches the sequential verdict byte-for-byte (0 = auto).
        for workers in [0, 1, 2, 4, 8] {
            let parallel = validate_chunks_parallel(source, &plan, workers);
            assert_eq!(parallel, sequential, "workers {workers}");
        }

        // A custom checker through the parallel factory seam — the
        // real per-chunk scan, so the verdict matches the built-in's.
        let custom = validate_chunks_parallel_with(source, &plan, 4, || Probe {
            reject: None,
            calls: 0,
        });
        assert_eq!(custom, sequential);
        // Inject a synthetic fault in EVERY chunk (each fresh checker
        // rejects its own first — and only — chunk): the report must
        // carry one attributed fault per chunk in scan order, proving
        // attribution and isolation both survive the fan-out.
        if plan.len() > 1 {
            let injected = validate_chunks_parallel_with(source, &plan, 4, || Probe {
                reject: Some(1),
                calls: 0,
            });
            assert_eq!(injected.faults.len(), plan.len(), "every chunk faulted");
            for (idx, fault) in injected.faults.iter().enumerate() {
                assert_eq!(fault.chunk, idx, "attribution stays in scan order");
                assert_eq!(fault.fault, StructuralFault::UnclosedBraces(1));
            }
        }

        // Plan + drive in one call, parallel form of `validate_plan`.
        let planned = validate_plan_parallel(source, &index, 4, 4);
        assert_eq!(planned, validate_plan(source, &index, 4));
    }

    // The wall-clock shape the P4 gate measures: a document with a
    // depth-0 boundary between every block (top-level comment) so the
    // 8-chunk target plans real parallel work.
    let mut big = String::with_capacity(12_000 * 48);
    for index in 0..12_000 {
        let _ = std::fmt::Write::write_fmt(
            &mut big,
            format_args!("{{\"k\": {index}, \"s\": \"xxxxxx\"}}\n// note\n"),
        );
    }
    let index = scan(&big);
    let plan = plan_chunks(&big, &index, 8);
    assert!(
        plan.len() >= 2,
        "the big document must plan multiple chunks"
    );
    let start = Instant::now();
    let sequential = validate_chunks(&big, &plan);
    let sequential_time = start.elapsed();
    let start = Instant::now();
    let parallel = validate_chunks_parallel(&big, &plan, 0);
    let parallel_time = start.elapsed();
    assert_eq!(parallel, sequential);
    println!(
        "12k-block document, {} chunks: sequential {sequential_time:?}, parallel(auto) {parallel_time:?} — verdicts identical",
        plan.len()
    );
    println!(
        "region-parallel drive: scoped workers, fresh validator per chunk, scan-order merge — parity holds"
    );
}
