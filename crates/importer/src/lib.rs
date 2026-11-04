//! # importer — tree-sitter `grammar.js` → gir KDL converter (issue 037)
//!
//! A conformance wedge, not a general JS interpreter: the importer
//! recovers the grammar object literal out of a `grammar.js` source,
//! converts the supported subset (`choice`, `seq`, `optional`,
//! `repeat`, `repeat1`, `field`, `alias`, `token`, `prec.left/right`,
//! literals, regexes, `extras`, `externals`) into a
//! [`gir_syntax::GirDoc`], and renders canonical KDL — while reporting
//! every mapping, drop, and note with a reason, deterministically.
//!
//! ```no_run
//! use importer::import;
//!
//! let src = r#"
//! module.exports = grammar({
//!   name: 'mini',
//!   rules: { start: $ => seq('a', repeat($.rest)), rest: $ => 'b' },
//! });
//! "#;
//! let out = import(src).expect("subset converts");
//! let report = out.report.to_string();
//! assert!(report.contains("mapped"), "{report}");
//! ```
//!
//! Nothing dropped ever dangles: rules that fail to convert are omitted
//! from the grammar, so the emitted KDL always loads through
//! [`gir_syntax::load`].

pub mod convert;
pub mod lexer;
pub mod parser;
pub mod wedge;

use std::fmt;

use gir_syntax::GirDoc;

pub use wedge::{KindDiff, diff_kinds, kind_multiset};

/// What the importer did with a construct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Converted into gir surface (possibly with sugar).
    Mapped,
    /// Not representable; omitted from the grammar.
    Dropped,
    /// Converted with a simplification worth knowing about.
    Note,
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mapped => f.write_str("mapped"),
            Self::Dropped => f.write_str("dropped"),
            Self::Note => f.write_str("note"),
        }
    }
}

/// One conversion-report line: what, where, what happened, why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportEntry {
    /// Construct kind (`rule`, `field`, `extras`, `externals`, …).
    pub construct: &'static str,
    /// Where it happened (rule name, token name, grammar name).
    pub scope: String,
    /// The action taken.
    pub action: Action,
    /// Human-readable explanation.
    pub detail: String,
}

/// The conversion report: entries in source traversal order
/// (deterministic by construction — no map iteration anywhere).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// Entries in traversal order.
    pub entries: Vec<ReportEntry>,
}

impl ImportReport {
    /// Append an entry.
    pub fn push(&mut self, entry: ReportEntry) {
        self.entries.push(entry);
    }

    /// Number of dropped constructs.
    #[must_use]
    pub fn dropped(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.action == Action::Dropped)
            .count()
    }

    /// Whether an entry exists with this action / construct / scope.
    #[must_use]
    pub fn has(&self, action: Action, construct: &str, scope: &str) -> bool {
        self.entries.iter().any(|entry| {
            entry.action == action && entry.construct == construct && entry.scope == scope
        })
    }

    /// Whether any entry's detail contains `needle`.
    #[must_use]
    pub fn contains_detail(&self, needle: &str) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.detail.contains(needle))
    }
}

impl fmt::Display for ImportReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mapped = self
            .entries
            .iter()
            .filter(|entry| entry.action == Action::Mapped)
            .count();
        writeln!(
            f,
            "import report: {mapped} mapped, {} dropped, {} notes",
            self.dropped(),
            self.entries.len() - mapped - self.dropped()
        )?;
        for entry in &self.entries {
            let scope = if entry.scope.is_empty() {
                String::new()
            } else {
                format!(" [{}]", entry.scope)
            };
            writeln!(
                f,
                "  {:8} {}{}: {}",
                entry.action, entry.construct, scope, entry.detail
            )?;
        }
        Ok(())
    }
}

/// The import result: the surface document plus the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportOutput {
    /// The converted grammar surface.
    pub doc: GirDoc,
    /// The conversion report.
    pub report: ImportReport,
}

/// An import failure: either a source parse error with its offset, or a
/// missing `module.exports = grammar({...})` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportError {
    /// The grammar.js source is malformed.
    Parse {
        /// Byte offset of the offending token.
        offset: usize,
        /// What went wrong.
        message: String,
    },
    /// No `module.exports = grammar({...})` statement was found.
    NoGrammar,
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse { offset, message } => write!(f, "parse error at byte {offset}: {message}"),
            Self::NoGrammar => {
                f.write_str("no `module.exports = grammar({...})` statement found in the source")
            }
        }
    }
}

impl std::error::Error for ImportError {}

/// Import a tree-sitter `grammar.js` source (the supported subset) into
/// a gir surface document plus the conversion report.
///
/// # Errors
/// [`ImportError::Parse`] on malformed sources, [`ImportError::NoGrammar`]
/// when the grammar statement is missing.
pub fn import(src: &str) -> Result<ImportOutput, ImportError> {
    let tokens = lexer::lex(src)?;
    let module = parser::parse_module(&tokens)?;
    let (doc, report) = convert::convert(&module);
    Ok(ImportOutput { doc, report })
}

/// Import and render canonical KDL in one step.
///
/// # Errors
/// Same as [`import`].
pub fn kdl(src: &str) -> Result<String, ImportError> {
    let out = import(src)?;
    Ok(gir_syntax::render(&out.doc))
}
