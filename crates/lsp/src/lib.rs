//! # lsp — the language servers (§9.4 + roadmap p3d)
//!
//! Two servers over one spine:
//!
//! - `gir-lsp` — the `.gir` grammar language server (issue 041):
//!   completion of rules and tokens, hover on annotations, inline
//!   conflict squiggles, go-to-references, rename with stable-ID
//!   preservation ([`analysis`]).
//! - `pack-lsp` — the pack language server (issue 042, roadmap §12.2
//!   p3d "LSP for packs"): diagnostics, outline symbols, folding,
//!   semantic tokens, highlights, and selection ranges over the green
//!   trees the shipped packs parse ([`pack`]).
//!
//! Posture: std-only, hand-rolled LSP 3.17 subset over JSON-RPC 2.0 —
//! the same thin-wrapper discipline as the playground server (no async
//! runtime, no external LSP SDK).
//!
//! - [`rpc`] — `Content-Length` framing + the JSON-RPC envelope layer.
//! - [`index`] — byte ↔ UTF-16 position mapping and word scanning.
//! - [`analysis`] — the grammar document model: symbols, references,
//!   hover, completion, rename, diagnostics.
//! - [`server`] — the grammar server workspace + dispatch table.
//! - [`pack`] — the pack server (registry, document views, dispatch).
//! - [`transport`] — the stdio serve loop (shared by both servers).
//!
//! ```no_run
//! use lsp::transport;
//!
//! // `syntact lsp` serves the editor over stdin/stdout.
//! let exit = transport::serve(
//!     &mut std::io::stdin().lock(),
//!     &mut std::io::stdout().lock(),
//!     transport::DEFAULT_MAX_MESSAGE,
//! );
//! ```

pub mod analysis;
pub mod index;
pub mod pack;
pub mod rpc;
pub mod server;
pub mod transport;

pub use analysis::{
    CompletionItem, Decl, DeclKind, Diag, DiagSeverity, Document, Hover, HoverContents, Location,
    RefSite, SymbolInfo, Symbols, Workspace, WorkspaceEdit,
};
pub use index::{LineIndex, Position, Range};
pub use pack::{
    DocumentHighlight, FoldingRange, Outline, PackServer, PackSymbol, SEMANTIC_TOKEN_TYPES,
};
pub use rpc::{Id, Incoming};
