//! # lsp — `gir-lsp`, the `.gir` grammar language server (§9.4, issue 041)
//!
//! The language server for grammar authoring: completion of rules and
//! tokens, hover on annotations, inline conflict squiggles (the
//! determinism report plus the LR(1) shadow automaton), go-to-references
//! across grammar files, and rename with stable-ID preservation.
//!
//! Posture: std-only, hand-rolled LSP 3.17 subset over JSON-RPC 2.0 —
//! the same thin-wrapper discipline as the playground server (no async
//! runtime, no external LSP SDK).
//!
//! - [`rpc`] — `Content-Length` framing + the JSON-RPC envelope layer.
//! - [`index`] — byte ↔ UTF-16 position mapping and word scanning.
//! - [`analysis`] — the document model: symbols, references, hover,
//!   completion, rename, diagnostics.
//! - [`server`] — workspace state + the method dispatch table.
//! - [`transport`] — the stdio serve loop.
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
pub mod rpc;
pub mod server;
pub mod transport;

pub use analysis::{
    CompletionItem, Decl, DeclKind, Diag, DiagSeverity, Document, Hover, HoverContents, Location,
    RefSite, SymbolInfo, Symbols, Workspace, WorkspaceEdit,
};
pub use index::{LineIndex, Position, Range};
pub use rpc::{Id, Incoming};
