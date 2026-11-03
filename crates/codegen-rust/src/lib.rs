//! # codegen-rust — deterministic O(1) Rust table emission (§4.3, issue 015)
//!
//! The table backend of the parser compiler: dense tables go in, stable
//! Rust source comes out.
//!
//! - [`CompactTable`] — row-displacement encoding of sparse `u16` tables:
//!   rows are placed at the first free base offset (deterministic
//!   first-fit), lookups are two array reads — O(1), branch-free on the
//!   happy path.
//! - [`Emitter`] — emission scaffolding with the readability contract
//!   (ADR-013): output is stable under regeneration (no timestamps, fixed
//!   wrapping, `#[rustfmt::skip]` blocks), so generated diffs only show
//!   real grammar changes.

pub mod emit;
pub mod table;

pub use emit::Emitter;
pub use table::CompactTable;
