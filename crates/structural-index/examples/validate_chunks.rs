//! The chunk-validation seam as a consumer: plan balanced chunks from
//! the region index, validate them independently through
//! [`ChunkValidator`] (the built-in fail-fast scanner, then a custom
//! counting checker riding the same seam), and read the merged report
//! — clean verdicts and faults attributed to their chunk with global
//! offsets, in parity with the whole-source `validate_fast`.
//!
//! Run: `cargo run -p structural-index --example validate_chunks`

use structural_index::{
    ChunkReport, ChunkValidator, StructuralFault, plan_chunks, scan, validate_chunks,
    validate_chunks_with, validate_fast, validate_plan,
};

/// A custom per-chunk checker riding the seam: records what it saw and
/// fails one chosen chunk (any host-specific check stands in here).
struct Probe {
    reject: Option<usize>,
    chunk_lens: Vec<usize>,
}

impl ChunkValidator for Probe {
    fn validate_chunk(&mut self, text: &str) -> Result<(), StructuralFault> {
        self.chunk_lens.push(text.len());
        if self.reject == Some(self.chunk_lens.len() - 1) {
            return Err(StructuralFault::UnclosedBraces(1));
        }
        Ok(())
    }
}

fn show(source: &str, report: &ChunkReport) {
    println!(
        "{source:?}: {}",
        if report.is_clean() {
            "clean"
        } else {
            "faulted"
        }
    );
    match report.first_fault() {
        Some(fault) => println!("  first fault: chunk {}, {:?}", fault.chunk, fault.fault),
        None => println!("  chunks: {}, no faults", report.chunks),
    }
}

fn main() {
    let sources = [
        "fn main() { let x = \"}\"; }\nfn second() { let y = \"(\"; }\n",
        "let s = \"unterminated {",
        "{ \"s\" } } x",
    ];
    for source in sources {
        // Plan + validate in one call: parity with the whole-source scan.
        let index = scan(source);
        let report = validate_plan(source, &index, 2);
        show(source, &report);
        assert_eq!(
            report.is_clean(),
            validate_fast(source).is_ok(),
            "chunked verdict matches the whole-source scan"
        );

        // The same plan through the driver, with a custom checker on
        // the seam — fault attribution and offset rebasing included.
        let plan = plan_chunks(source, &index, 2);
        let mut probe = Probe {
            reject: None,
            chunk_lens: Vec::new(),
        };
        let custom = validate_chunks_with(source, &plan, &mut probe);
        assert!(custom.is_clean(), "the probe accepts every chunk");
        assert_eq!(probe.chunk_lens.len(), plan.len(), "one call per chunk");
        assert_eq!(
            probe.chunk_lens.iter().sum::<usize>(),
            source.len(),
            "chunks tile the source"
        );

        // The built-in scanner over a precomputed plan (the pure
        // per-chunk cost).
        let builtin = validate_chunks(source, &plan);
        assert_eq!(builtin, report, "same plan, same verdict");
    }
    println!("chunked validation: plan, validate per chunk, merge — parity holds");
}
