//! The validate fast path + region-parallel chunk planning (issue 032).

use crate::index::{RegionIndex, RegionKind};
use crate::scan::scan;

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
/// the fast path: one block scan, no allocation beyond the scan state.
/// Returns the first fault, or `None` when the source is balanced.
///
/// # Errors
/// Returns the first [`StructuralFault`] encountered.
pub fn validate_fast(source: &str) -> Result<(), StructuralFault> {
    let index = scan(source);
    validate_from_index(&index, source)
}

/// The index-driven validator (the fast path shares the scan; this is
/// the pure classification-to-verdict step).
fn validate_from_index(index: &RegionIndex, source: &str) -> Result<(), StructuralFault> {
    let source_len = u32::try_from(source.len()).unwrap_or(u32::MAX);
    let mut depth: u8 = 0;
    for (kind, start, end, region_depth) in index.iter() {
        match kind {
            RegionKind::Code => {
                // The region's own depth deltas happened during the scan;
                // `depths[]` records the entry depth — recompute the exit
                // depth by counting braces in the region.
                let clipped_end =
                    (end as usize).min(usize::try_from(source_len).unwrap_or(usize::MAX));
                for (offset, byte) in source.as_bytes()[start as usize..clipped_end]
                    .iter()
                    .enumerate()
                {
                    match byte {
                        b'{' => depth = depth.saturating_add(1),
                        b'}' if depth == 0 => {
                            let at = start as usize + offset;
                            return Err(StructuralFault::StrayClose(
                                u32::try_from(at).unwrap_or(0),
                            ));
                        }
                        b'}' => depth = depth.saturating_sub(1),
                        _ => {}
                    }
                }
            }
            RegionKind::StrDouble | RegionKind::StrSingle | RegionKind::StrBacktick => {
                // A string region ends at its closing quote unless it hit
                // EOF: unterminated when it runs to the very end without
                // the quote.
                if end == source_len {
                    let bytes = source.as_bytes();
                    let last = bytes.get(end as usize - 1);
                    if last != Some(&quote_byte(kind)) {
                        return Err(StructuralFault::UnterminatedString(start));
                    }
                }
            }
            RegionKind::LineComment => {}
            RegionKind::BlockComment => {
                if end == source_len {
                    let bytes = source.as_bytes();
                    if bytes.get(end as usize - 1) != Some(&b'/') {
                        return Err(StructuralFault::UnterminatedComment(start));
                    }
                }
            }
        }
        let _ = region_depth;
    }
    if depth != 0 {
        return Err(StructuralFault::UnclosedBraces(depth));
    }
    Ok(())
}

fn quote_byte(kind: RegionKind) -> u8 {
    match kind {
        RegionKind::StrDouble => b'"',
        RegionKind::StrSingle => b'\'',
        RegionKind::StrBacktick => b'`',
        _ => 0,
    }
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

/// Plan `target` contiguous chunks whose boundaries all sit at depth-0
/// code positions (never inside a string or comment) — each chunk is
/// safe to parse in parallel. Deterministic: boundaries are the
/// depth-0 code-region starts nearest the even split points.
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
    // Candidate boundaries: 0, len, and every depth-0 CODE region start
    // (those are provably outside strings/comments and at depth 0).
    let mut candidates: Vec<u32> = Vec::new();
    candidates.push(0);
    for (kind, start, _end, depth) in index.iter() {
        if kind == RegionKind::Code && depth == 0 && start > 0 && start < len {
            candidates.push(start);
        }
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
}
