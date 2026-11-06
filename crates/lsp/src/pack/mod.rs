//! # pack — `pack-lsp`, the language server over the shipped packs
//! (roadmap §12.2 p3d "LSP for packs", issue 042)
//!
//! Where [`crate::analysis`] serves grammar *authoring*, this module
//! serves the languages the packs *parse*: a registry maps documents to
//! their pack, one pack `parse` per document version produces the green
//! tree, a single DFS materializes a parent-indexed outline, and every
//! tree view (diagnostics, symbols, folding, semantic tokens,
//! highlights, selection ranges) derives from it — computed once per
//! version, read at query time.
//!
//! - [`registry`] — pack entries and uri/language resolution.
//! - [`outline`] — the positioned tree index every view derives from.
//! - [`document`] — the document model and its prebuilt views.
//! - [`server`] — the lifecycle state machine + dispatch table.

pub mod document;
pub mod outline;
pub mod registry;
pub mod server;

pub use document::{Document, DocumentHighlight, FoldingRange, PackSymbol};
pub use outline::Outline;
pub use server::{PackServer, SEMANTIC_TOKEN_TYPES};
