//! # `structural-index` — block scanner + `SoA` region index (issue 032,
//! §7.9, ADR-011)
//!
//! A 64-byte block scanner classifies source bytes into quote, brace,
//! comment, and escape regions, recording them in a flat structure-of-
//! arrays [`index::RegionIndex`] at a hard ~16 B/region budget, plus a
//! validate fast path and a region-parallel chunk planning API.
//!
//! # Documented design (ADR-011)
//!
//! - **Block scanning**: the source is processed in 64-byte blocks; in
//!   the `Code` state [`memchr`] locates the next interesting byte
//!   (quote, `/`, `{`, `}`, `\`) and every byte before it is batched
//!   into the current code region — the chunking stands in for true
//!   SIMD scanning.
//! - **States**: `Code`, `"…"`, `'…'`, backtick strings, `// …` line
//!   comments, `/* … */` block comments; `\` escapes skip one byte
//!   inside strings; `{`/`}` adjust the brace depth only in `Code`.
//! - **`SoA` index**: four parallel vectors (`kind: u8`, `start: u32`,
//!   `end: u32`, `depth: u8`) — 10 B of payload per region; the
//!   [`index::RegionIndex::bytes_per_region`] accounting (capacities
//!   included) must stay ≤ 16 B/region, asserted by a test.
//! - **Validate fast path**: [`plan::validate_fast`] runs the same
//!   block scan without building the index and reports the first
//!   structural fault (unterminated quote/comment, unbalanced brace)
//!   with its offset.
//! - **Region sink seam**: [`sink::RegionSink`] consumes regions as
//!   they close ([`sink::scan_regions`], `ControlFlow` early exit);
//!   [`sink::FaultScanner`] is the built-in streaming, fail-fast
//!   validator behind [`plan::validate_fast`].
//! - **Chunk validation seam**: [`chunks::ChunkValidator`] validates
//!   one planned chunk in isolation; [`chunks::validate_chunks`] /
//!   [`chunks::validate_plan`] merge the per-chunk verdicts into an
//!   attributed [`chunks::ChunkReport`] (parity with
//!   [`plan::validate_fast`] pinned by tests) — the shape the
//!   region-parallel roadmap (P4) runs on.
//! - **Chunk planning**: [`plan::plan_chunks`] cuts the source into
//!   contiguous, balanced, deterministic chunks whose boundaries sit at
//!   depth-0 code positions — each chunk is independently parseable.

pub mod chunks;
pub mod index;
pub mod plan;
pub mod scan;
pub mod sink;

pub use chunks::{
    ChunkFault, ChunkFaultScanner, ChunkReport, ChunkValidator, validate_chunks,
    validate_chunks_with, validate_plan,
};
pub use index::{RegionIndex, RegionKind};
pub use plan::{ChunkPlan, StructuralFault, plan_chunks, validate_fast};
pub use scan::{scan, scan_reference};
pub use sink::{FaultScanner, RegionSink, scan_regions};
