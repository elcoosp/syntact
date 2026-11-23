//! # codegen-rust — deterministic Rust emission (§4.3, §6.1 pass 12, issue 045)
//!
//! The parser-compiler's emission stage: dense descriptors go in, stable
//! Rust source comes out.
//!
//! Two backends:
//!
//! - [`table`] / [`emit::Emitter`] — the O(1) row-displacement table backend
//!   (issue 015): sparse `u16` tables compress to one flat array + base
//!   offsets; the readability contract (ADR-013) keeps emission diff-stable.
//! - [`descent`] — the descent backend (issue 045): takes a
//!   [`descent_lower::Lowered`] program + a [`lexgen::Scanner`] and emits a
//!   standalone, compiling, safe-Rust parser crate that reproduces
//!   `descent::engine` output byte-for-byte. Closes the compiler loop the
//!   roadmap promises (§0, §12.3 P0 "compiling crate" row).

pub mod descent;
pub mod emit;
pub mod table;

pub use descent::{EmitError, EmittedCrate, EmittedFile, emit_crate};
pub use emit::Emitter;
pub use table::CompactTable;
