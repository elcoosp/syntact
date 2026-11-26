//! The chunk-validation seam: consume the balanced chunk plans
//! [`crate::plan::plan_chunks`] cuts. Every boundary sits at a true
//! depth-0 code position, so each chunk is independently validatable —
//! validation decomposes per chunk, the shape region-parallel
//! validation (roadmap P4) runs on, and the [`ChunkValidator`]
//! contract is the seam hosts plug their own per-chunk checkers into.
//!
//! [`validate_chunks`] drives one validator per chunk and merges the
//! verdicts into a [`ChunkReport`]: faults keep their chunk
//! attribution, and string/comment/stray-close offsets are rebased
//! into source coordinates (a validator only ever saw its chunk's own
//! bytes). Verdict parity with the whole-source
//! [`validate_fast`](crate::validate_fast) is pinned by the tests: the
//! chunked report is clean exactly when the whole source validates
//! clean, and its first fault is the whole-source fault.

use crate::index::RegionIndex;
use crate::plan::{ChunkPlan, StructuralFault, plan_chunks};
use crate::sink::{FaultScanner, scan_regions};

/// One chunk's fault: which chunk failed, and the fault with its
/// offsets rebased to source coordinates (the validator saw only the
/// chunk's own bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkFault {
    /// The plan index of the chunk that faulted.
    pub chunk: usize,
    /// The fault (string/comment/stray-close offsets are global).
    pub fault: StructuralFault,
}

/// The merged chunked-validation verdict: how many chunks ran and what
/// they found. Chunks are contiguous and planned in scan order, so
/// `faults` is in scan order — `first_fault` is the whole-source first
/// fault, the exact verdict `validate_fast` returns, now attributed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkReport {
    /// How many chunks the plan supplied (all were validated).
    pub chunks: usize,
    /// Per-chunk faults in chunk (scan) order.
    pub faults: Vec<ChunkFault>,
}

impl ChunkReport {
    /// True when every chunk validated clean — equivalent to the whole
    /// source validating clean (parity pinned by the tests).
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.faults.is_empty()
    }

    /// The first fault in scan order (chunks are contiguous and
    /// ordered), with its chunk attribution — the chunked form of the
    /// `validate_fast` verdict.
    #[must_use]
    pub fn first_fault(&self) -> Option<ChunkFault> {
        self.faults.first().copied()
    }
}

/// The per-chunk validation contract: validate ONE chunk in isolation.
/// `text` is exactly the chunk's byte range — a balanced slice, since
/// `plan_chunks` boundaries sit at true depth-0 positions — so an
/// implementation sees nothing outside its chunk: the isolation the
/// region-parallel drive relies on. Reported fault offsets are
/// chunk-local; the driver rebases them to source coordinates.
pub trait ChunkValidator {
    /// Validate the chunk's own bytes; the fault (if any) carries
    /// chunk-local offsets.
    ///
    /// # Errors
    /// The chunk's first [`StructuralFault`], in the
    /// [`validate_fast`](crate::validate_fast) shape.
    fn validate_chunk(&mut self, text: &str) -> Result<(), StructuralFault>;
}

/// The built-in [`ChunkValidator`]: a fresh fail-fast [`FaultScanner`]
/// per chunk — the streaming validator applied per chunk, no index
/// materialization anywhere on the path.
#[derive(Debug, Clone, Copy)]
pub struct ChunkFaultScanner;

impl ChunkValidator for ChunkFaultScanner {
    fn validate_chunk(&mut self, text: &str) -> Result<(), StructuralFault> {
        let mut scanner = FaultScanner::new(text);
        let _ = scan_regions(text, &mut scanner);
        scanner.finish()
    }
}

/// Clip a chunk's bounds to the source and borrow its exact byte
/// range — the clipping discipline BOTH the sequential and the
/// region-parallel drives apply, so their verdicts stay comparable:
/// out-of-range chunk bounds clip to the source (the same discipline
/// the scanner applies to region records); a bound that does not land
/// on a char boundary scans as empty — unreachable for `plan_chunks`
/// plans, whose boundaries always follow an ASCII byte.
pub(crate) fn chunk_text<'s>(source: &'s str, chunk: &ChunkPlan) -> &'s str {
    let bytes = source.as_bytes();
    let start = (chunk.start as usize).min(bytes.len());
    let end = (chunk.end as usize).min(bytes.len()).max(start);
    std::str::from_utf8(&bytes[start..end]).unwrap_or("")
}

/// Validate every chunk of `plan` with `validator`, merging the
/// verdicts. Chunk-local fault offsets are rebased into source
/// coordinates; the attribution points at the plan index the fault
/// came from.
#[must_use]
pub fn validate_chunks_with(
    source: &str,
    plan: &[ChunkPlan],
    validator: &mut dyn ChunkValidator,
) -> ChunkReport {
    let mut faults = Vec::new();
    for (idx, chunk) in plan.iter().enumerate() {
        if let Err(fault) = validator.validate_chunk(chunk_text(source, chunk)) {
            faults.push(ChunkFault {
                chunk: idx,
                fault: rebase(fault, chunk.start),
            });
        }
    }
    ChunkReport {
        chunks: plan.len(),
        faults,
    }
}

/// Validate every chunk of `plan` with the built-in
/// [`ChunkFaultScanner`].
#[must_use]
pub fn validate_chunks(source: &str, plan: &[ChunkPlan]) -> ChunkReport {
    validate_chunks_with(source, plan, &mut ChunkFaultScanner)
}

/// Plan and validate in one call: [`plan_chunks`] cuts `target`
/// balanced chunks out of the index, then [`validate_chunks`] runs
/// them.
#[must_use]
pub fn validate_plan(source: &str, index: &RegionIndex, target: usize) -> ChunkReport {
    validate_chunks(source, &plan_chunks(source, index, target))
}

/// Rebase a chunk-local fault into source coordinates
/// (`UnclosedBraces` carries no offset). Chunk-local offsets never
/// exceed the chunk length, so the add cannot outrun the source.
pub(crate) fn rebase(fault: StructuralFault, base: u32) -> StructuralFault {
    match fault {
        StructuralFault::UnterminatedString(at) => {
            StructuralFault::UnterminatedString(at.saturating_add(base))
        }
        StructuralFault::UnterminatedComment(at) => {
            StructuralFault::UnterminatedComment(at.saturating_add(base))
        }
        StructuralFault::StrayClose(at) => StructuralFault::StrayClose(at.saturating_add(base)),
        StructuralFault::UnclosedBraces(depth) => StructuralFault::UnclosedBraces(depth),
    }
}
