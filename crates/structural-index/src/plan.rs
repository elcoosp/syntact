//! The validate fast path + region-parallel chunk planning (issue 032).

use crate::index::{RegionIndex, RegionKind};
use crate::sink::FaultScanner;

/// A structural fault found by the validate fast path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuralFault {
    /// An unterminated string starting at `offset`.
    UnterminatedString(u32),
    /// An unterminated block comment starting at `offset`.
    UnterminatedComment(u32),
    /// A stray `}` at `offset` (depth went negative).
    StrayClose(u32),
    /// EOF at nonzero brace depth `depth`.
    UnclosedBraces(u8),
}

/// Validate `source` structurally WITHOUT building the region index —
/// the fast path: one streaming block scan into a
/// [`FaultScanner`], no allocation beyond the O(1) scan state, and the
/// scan stops at the first fault. Returns the first fault, or `None`
/// when the source is balanced.
///
/// # Errors
/// Returns the first [`StructuralFault`] encountered.
pub fn validate_fast(source: &str) -> Result<(), StructuralFault> {
    let mut scanner = FaultScanner::new(source);
    let _ = crate::sink::scan_regions(source, &mut scanner);
    scanner.finish()
}

/// One planned chunk: a contiguous, balanced, independently parseable
/// byte range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkPlan {
    /// Chunk start offset.
    pub start: u32,
    /// Chunk end offset (exclusive).
    pub end: u32,
    /// The brace depth at the chunk start (always 0 for a balanced
    /// plan).
    pub depth: u8,
}

/// Plan `target` contiguous chunks whose boundaries all sit at true
/// depth-0 code positions (never inside a string or comment, never at
/// a nonzero brace depth) — each chunk is balanced and safe to parse
/// in parallel. Deterministic: boundaries are the depth-0 code-region
/// starts nearest the even split points.
///
/// The depth a boundary sits at is the PREVIOUS region's end depth,
/// not the region's own record: the index records the depth at each
/// region's end (braces do not split Code regions), so a region like
/// ` } x` records 0 while the position it starts at is at depth 1.
///
/// # Panics
/// Never: the boundary list is seeded with `0`, so `last()` always
/// finds an element during merging.
#[must_use]
pub fn plan_chunks(source: &str, index: &RegionIndex, target: usize) -> Vec<ChunkPlan> {
    let len = u32::try_from(source.len()).unwrap_or(u32::MAX);
    if len == 0 {
        return Vec::new();
    }
    let target = target.max(1);
    // Candidate boundaries: 0, len, and every Code region START whose
    // actual brace depth is 0 (those are provably outside
    // strings/comments and at depth 0). The actual depth at a region
    // start is the previous record's depth — depth only changes inside
    // Code regions, so each record's depth is the next region's
    // starting depth.
    let mut candidates: Vec<u32> = Vec::new();
    candidates.push(0);
    let mut prev_depth: u8 = 0;
    for (kind, start, _end, depth) in index.iter() {
        if kind == RegionKind::Code && prev_depth == 0 && start > 0 && start < len {
            candidates.push(start);
        }
        prev_depth = depth;
    }
    candidates.push(len);
    candidates.sort_unstable();
    candidates.dedup();
    if candidates.len() <= 2 || target == 1 {
        return vec![ChunkPlan {
            start: 0,
            end: len,
            depth: 0,
        }];
    }
    // Pick `target - 1` interior candidates nearest the even splits.
    let mut boundaries: Vec<u32> = vec![0];
    for i in 1..target {
        let split = usize::try_from(len).unwrap_or(0) * i / target;
        let chosen = candidates
            .iter()
            .copied()
            .filter(|c| *c > 0 && *c < len)
            .min_by_key(|c| c.abs_diff(u32::try_from(split).unwrap_or(u32::MAX)))
            .unwrap_or(len);
        if *boundaries.last().unwrap() != chosen {
            boundaries.push(chosen);
        }
    }
    boundaries.push(len);
    boundaries.dedup();
    boundaries
        .windows(2)
        .map(|pair| ChunkPlan {
            start: pair[0],
            end: pair[1],
            depth: 0,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{StructuralFault, plan_chunks, validate_fast};
    use crate::scan::scan;

    #[test]
    fn validate_accepts_balanced_sources() {
        assert_eq!(validate_fast("fn main() { let x = \"}\"; }"), Ok(()));
        assert_eq!(validate_fast("// { unbalanced in comment"), Ok(()));
        assert_eq!(validate_fast("/* } */"), Ok(()));
        assert_eq!(validate_fast(""), Ok(()));
    }

    #[test]
    fn validate_reports_faults_with_offsets() {
        assert_eq!(
            validate_fast("let s = \"unterminated"),
            Err(StructuralFault::UnterminatedString(8))
        );
        assert_eq!(
            validate_fast("/* open"),
            Err(StructuralFault::UnterminatedComment(0))
        );
        assert_eq!(validate_fast("a } b"), Err(StructuralFault::StrayClose(2)));
        assert_eq!(
            validate_fast("a { b"),
            Err(StructuralFault::UnclosedBraces(1))
        );
    }

    #[test]
    fn chunks_are_contiguous_and_balanced() {
        let source = "fn a() { x }\nfn b() { y }\nfn c() { z }\nfn d() { w }\n";
        let index = scan(source);
        let plans = plan_chunks(source, &index, 2);
        assert_eq!(plans.first().map(|p| p.start), Some(0));
        assert_eq!(
            plans.last().map(|p| p.end),
            Some(u32::try_from(source.len()).unwrap_or(0))
        );
        for pair in plans.windows(2) {
            assert_eq!(pair[0].end, pair[1].start, "contiguous");
        }
        for plan in &plans {
            assert_eq!(plan.depth, 0, "every chunk starts at depth 0");
            // True balance: the chunk's own bytes validate clean.
            let bytes = &source[plan.start as usize..plan.end as usize];
            assert_eq!(validate_fast(bytes), Ok(()), "chunk {plan:?} unbalanced");
            // No chunk boundary falls inside a string/comment region.
            for (kind, start, end, _depth) in index.iter() {
                if kind != crate::RegionKind::Code {
                    assert!(
                        end <= plan.start || start >= plan.end,
                        "region {start}..{end} not contained in chunk {plan:?}"
                    );
                }
            }
        }
    }

    /// The trap the candidate filter must avoid: the ` } x` region
    /// records depth 0 (its end depth) while the position it starts at
    /// sits at depth 1 — a boundary there would cut `{ "s" ` into an
    /// unbalanced chunk. Pinned by `tests/chunks.rs` too; this is the
    /// module-local smallest repro.
    #[test]
    fn boundaries_use_the_boundary_depth_not_the_region_record() {
        let source = "{ \"s\" } x";
        let index = scan(source);
        for target in [1, 2, 3, 4] {
            for plan in plan_chunks(source, &index, target) {
                let bytes = &source[plan.start as usize..plan.end as usize];
                assert_eq!(
                    validate_fast(bytes),
                    Ok(()),
                    "chunk {plan:?} of {source:?} (target {target}) is not balanced"
                );
            }
        }
    }
}
