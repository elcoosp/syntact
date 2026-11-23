//! # generated-json — the JSON fixture emitted as a workspace crate (issue 045)
//!
//! The codegen-rust descent backend emits a standalone compiling parser
//! crate from the JSON `.gir` fixture. To make the generated parser
//! benchmarkable in the workspace (criterion + dhat gates), `build.rs`
//! runs the emitter and writes the generated modules into `OUT_DIR`;
//! this crate's `lib.rs` declares them with `#[path]` so the emitted
//! `//!`-documented files compile as normal modules. Every `cargo build`
//! regenerates the source, so the crate always reflects the current
//! `codegen-rust` + `descent-lower` (ADR-013: regeneration is diff-stable).
//!
//! This is **not** a hand-written pack — it is the emitted output, made a
//! workspace member so benches and the dhat budget gate can measure the
//! generated parser head-to-head against the engine and the reference.

// The build script writes the generated source into OUT_DIR; include it.
// The generated lib body declares the modules + the public API.
include!(concat!(env!("OUT_DIR"), "/generated_lib.rs"));
