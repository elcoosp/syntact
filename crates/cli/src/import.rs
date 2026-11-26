//! `syntact import` — the tree-sitter adoption wedge as a CLI
//! capability (roadmap §9.1 command list, ADR-015).
//!
//! The importer (issue 037) converts a tree-sitter `grammar.js` into
//! the gir surface with a provenance log — every construct is mapped,
//! dropped, or noted, with the reason. This module is the command
//! layer around it: the converted surface must load back through
//! `gir-syntax` before anything is written or reported (the issue 037
//! wedge gate: an import nobody can load is worthless), the report
//! rides alongside the KDL in both text and JSON form, and every
//! refusal is named.
//!
//! Determinism is the ADR-013 posture carried over from the importer
//! tests: the same source converts to the same KDL and the same
//! report, twice.

use std::fmt;

use importer::{ImportError, ImportReport};

/// One successful conversion: the canonical KDL surface plus the
/// provenance report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imported {
    /// The converted grammar surface (canonical KDL, rendered by
    /// `gir-syntax`).
    pub kdl: String,
    /// What happened to every construct (mapped/dropped/why).
    pub report: ImportReport,
}

/// A named command refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportCommandError {
    /// The tree-sitter source is malformed or has no
    /// `module.exports = grammar({...})` statement.
    Import(ImportError),
    /// The converted surface does not load back through `gir-syntax` —
    /// the wedge gate refuses to report or write an unusable import.
    Reload(String),
}

impl fmt::Display for ImportCommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Import(error) => write!(f, "import: {error}"),
            Self::Reload(reason) => write!(f, "imported grammar does not load: {reason}"),
        }
    }
}

impl std::error::Error for ImportCommandError {}

/// Import a tree-sitter `grammar.js` source; pure (no filesystem).
/// The converted surface is gated through `gir-syntax::load_grammar`
/// before it is returned — an import that cannot load is an error, not
/// a deliverable.
///
/// # Errors
/// [`ImportCommandError::Import`] when the source is malformed or has
/// no grammar statement, [`ImportCommandError::Reload`] when the
/// converted surface fails to load back (never observed on the
/// supported subset — the gate exists so it never ships silently).
pub fn import_document(source: &str) -> Result<Imported, ImportCommandError> {
    let output = importer::import(source).map_err(ImportCommandError::Import)?;
    let kdl = gir_syntax::render(&output.doc);
    if let Err(error) = gir_syntax::load_grammar(&kdl) {
        return Err(ImportCommandError::Reload(error.to_string()));
    }
    Ok(Imported {
        kdl,
        report: output.report,
    })
}

/// Render the conversion report as JSON (`--json`), in the
/// [`trace`](crate::trace) hand-rolled style: entries in traversal
/// order, then the totals.
#[must_use]
pub fn render_json(report: &ImportReport) -> String {
    use std::fmt::Write as _;
    let mapped = report
        .entries
        .iter()
        .filter(|entry| entry.action == importer::Action::Mapped)
        .count();
    let notes = report.entries.len() - mapped - report.dropped();
    let mut out = String::new();
    let _ = writeln!(out, "{{");
    let _ = writeln!(out, "  \"entries\": [");
    for (index, entry) in report.entries.iter().enumerate() {
        let comma = if index + 1 < report.entries.len() {
            ","
        } else {
            ""
        };
        let _ = writeln!(out, "    {{");
        let _ = writeln!(out, "      \"construct\": {},", json_str(entry.construct));
        let _ = writeln!(out, "      \"scope\": {},", json_str(&entry.scope));
        let _ = writeln!(out, "      \"action\": \"{}\",", entry.action);
        let _ = writeln!(out, "      \"detail\": {}", json_str(&entry.detail));
        let _ = writeln!(out, "    }}{comma}");
    }
    let _ = writeln!(out, "  ],");
    let _ = writeln!(out, "  \"mapped\": {mapped},");
    let _ = writeln!(out, "  \"dropped\": {},", report.dropped());
    let _ = writeln!(out, "  \"notes\": {notes}");
    let _ = writeln!(out, "}}");
    out
}

/// Escape one string as a JSON string literal.
fn json_str(value: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
