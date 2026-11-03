//! # gir-syntax — KDL surface loader for `.gir` grammar packages (§5)
//!
//! Issue 004: span-preserving parse, round-trip render, separated-list
//! sugar. The KDL text loads into a span-carrying [`GirDoc`]; the same
//! document lowers to the canonical IR ([`gir_ir::Grammar`]) and renders
//! back to canonical KDL.
//!
//! ```no_run
//! use gir_syntax::{load, load_grammar, render};
//!
//! let text = r#"
//! language "mini" version="1.0.0"
//! grammar {
//!     start kind=seq "a" start-tail opt
//!     start-tail kind=seq "b" start opt
//! }
//! "#;
//! let doc = load(text).expect("well-formed surface");
//! let grammar = load_grammar(text).expect("lowers to valid IR");
//! let again = load(&render(&doc)).expect("render is valid KDL again");
//! assert_eq!(again.to_grammar().expect("same IR"), grammar);
//! ```

pub mod doc;
pub mod error;
pub mod loader;
pub mod lower;
pub mod render;

pub use doc::{
    GirDoc, ItemSurf, LanguageMeta, Marker, RuleDecl, RuleKindSurf, SepCfg, SeqEl, TokenDecl,
    TokenKindSurf,
};
pub use error::{ByteSpan, SurfaceError};
pub use loader::{RESERVED_MARKERS, load};
pub use lower::{assoc_word, load_grammar};
pub use render::render;
