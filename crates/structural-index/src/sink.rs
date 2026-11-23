//! The region sink seam (§7.9): consume regions as they close instead
//! of materializing the full [`RegionIndex`] — the streaming shape the
//! validate fast path and the region-parallel planning (roadmap P4)
//! build on.
//!
//! [`scan_regions`] drives any [`RegionSink`] over the exact region
//! stream the block scanner records ([`crate::scan::scan`] is itself
//! just the index-collecting consumer), with
//! [`std::ops::ControlFlow`] early exit on the sink side — the same
//! discipline as the runtime-core push visitor. [`FaultScanner`] is
//! the built-in streaming validator: it classifies the first fault as
//! regions close and stops the scan, never building an index.

use std::ops::ControlFlow;

use crate::index::{RegionIndex, RegionKind};
use crate::plan::StructuralFault;

/// A consumer of completed regions: called once per region, in scan
/// order, with the same `(kind, start, end, depth)` records the
/// [`RegionIndex`] stores. Return [`ControlFlow::Break`] to stop the
/// scan early (fail-fast consumers); [`ControlFlow::Continue`] keeps
/// scanning.
pub trait RegionSink {
    /// One completed region.
    fn region(&mut self, kind: RegionKind, start: u32, end: u32, depth: u8) -> ControlFlow<()>;
}

/// Scan `source` with the 64-byte block scanner, driving each
/// completed region into `sink` as it closes. Regions arrive in scan
/// order with the same records `scan` records into an index; a
/// [`ControlFlow::Break`] from the sink stops the scan (the remaining
/// source bytes go unexamined).
///
/// Bit-parity with [`crate::scan::scan`]: driving an index collector
/// reproduces `scan`'s output exactly (pinned by the stream tests).
pub fn scan_regions(source: &str, sink: &mut dyn RegionSink) -> ControlFlow<()> {
    crate::scan::Scanner::new(source, sink).run()
}

impl RegionSink for RegionIndex {
    /// Record the region (the index-collecting consumer `scan` runs).
    fn region(&mut self, kind: RegionKind, start: u32, end: u32, depth: u8) -> ControlFlow<()> {
        self.push(kind, start, end, depth);
        ControlFlow::Continue(())
    }
}

/// The streaming fail-fast validator: the per-region fault logic of the
/// index-driven validate step, applied as regions close — no index
/// materialization, no per-region storage, O(1) state. The first fault
/// breaks the drive (later regions go unexamined); call
/// [`FaultScanner::finish`] after [`scan_regions`] for the verdict.
///
/// Memory: one `&str`, a depth byte, a fault slot, and a counter.
pub struct FaultScanner<'s> {
    /// The source under validation (Code regions are re-scanned for
    /// brace deltas, the same discipline as the index-driven step).
    source: &'s str,
    /// Brace depth accumulated across Code regions (saturating).
    depth: u8,
    /// The first fault found, if any.
    fault: Option<StructuralFault>,
    /// Regions consumed so far (the fail-fast observability probe).
    seen: usize,
}

impl<'s> FaultScanner<'s> {
    /// A validator over `source`.
    #[must_use]
    pub fn new(source: &'s str) -> Self {
        Self {
            source,
            depth: 0,
            fault: None,
            seen: 0,
        }
    }

    /// How many regions were consumed before the drive stopped — the
    /// observable fail-fast signal (a full, clean scan sees them all).
    #[must_use]
    pub fn regions_seen(&self) -> usize {
        self.seen
    }

    /// The final verdict: the first fault found during the drive, else
    /// `UnclosedBraces` when the accumulated depth ended nonzero (the
    /// EOF check the index-driven step performs last).
    ///
    /// # Errors
    /// Returns the first [`StructuralFault`] (drive fault or EOF
    /// depth).
    pub fn finish(self) -> Result<(), StructuralFault> {
        if let Some(fault) = self.fault {
            return Err(fault);
        }
        if self.depth != 0 {
            return Err(StructuralFault::UnclosedBraces(self.depth));
        }
        Ok(())
    }

    /// Apply one closed region's fault logic (the index-driven
    /// validate step's body, region by region).
    fn apply_region(&mut self, kind: RegionKind, start: u32, end: u32) -> ControlFlow<()> {
        self.seen += 1;
        let source_len = u32::try_from(self.source.len()).unwrap_or(u32::MAX);
        match kind {
            RegionKind::Code => {
                // The region's brace deltas are recomputed from the
                // source bytes (the index's `depths` record the scan
                // state, not the per-region delta the verdict needs).
                let clipped_end =
                    (end as usize).min(usize::try_from(source_len).unwrap_or(usize::MAX));
                for (offset, byte) in self.source.as_bytes()[start as usize..clipped_end]
                    .iter()
                    .enumerate()
                {
                    match byte {
                        b'{' => self.depth = self.depth.saturating_add(1),
                        b'}' if self.depth == 0 => {
                            let at = start as usize + offset;
                            self.fault =
                                Some(StructuralFault::StrayClose(u32::try_from(at).unwrap_or(0)));
                            return ControlFlow::Break(());
                        }
                        b'}' => self.depth = self.depth.saturating_sub(1),
                        _ => {}
                    }
                }
            }
            RegionKind::StrDouble | RegionKind::StrSingle | RegionKind::StrBacktick => {
                // A string region ends at its closing quote unless it
                // hit EOF: unterminated when it runs to the very end
                // without the quote.
                if end == source_len {
                    let bytes = self.source.as_bytes();
                    let last = bytes.get(end as usize - 1);
                    if last != Some(&quote_byte(kind)) {
                        self.fault = Some(StructuralFault::UnterminatedString(start));
                        return ControlFlow::Break(());
                    }
                }
            }
            RegionKind::LineComment => {}
            RegionKind::BlockComment => {
                if end == source_len {
                    let bytes = self.source.as_bytes();
                    if bytes.get(end as usize - 1) != Some(&b'/') {
                        self.fault = Some(StructuralFault::UnterminatedComment(start));
                        return ControlFlow::Break(());
                    }
                }
            }
        }
        ControlFlow::Continue(())
    }
}

impl RegionSink for FaultScanner<'_> {
    fn region(&mut self, kind: RegionKind, start: u32, end: u32, _depth: u8) -> ControlFlow<()> {
        self.apply_region(kind, start, end)
    }
}

/// The quote byte that terminates `kind`'s string region (0 for
/// non-string kinds).
fn quote_byte(kind: RegionKind) -> u8 {
    match kind {
        RegionKind::StrDouble => b'"',
        RegionKind::StrSingle => b'\'',
        RegionKind::StrBacktick => b'`',
        _ => 0,
    }
}
