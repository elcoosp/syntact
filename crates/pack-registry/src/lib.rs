//! # pack-registry — the canonical registry of the shipped packs (issue 046)
//!
//! One registration of the ten language packs, three lookup axes, and
//! the pack entry points as first-class values. Before this crate the
//! same roster was registered three times in three shapes: the pack LSP
//! kept a `PackRegistry` (10 packs), the playground server kept its own
//! `Registry` (7 packs — ini/csv/markdown were unreachable), and the CLI
//! hardcoded `json`/`toml` plus JSON-only check/conformance. Consumers
//! now share this single seam:
//!
//! - [`PackEntry`] carries the pack identity (`name`, `language_id`,
//!   `extensions`) plus the uniform entry points every shipped pack
//!   exposes (`parse`, `parse_events`, `error_message`).
//! - [`PackRegistry`] resolves packs by name, LSP `languageId`, file
//!   extension, or document URI — the same deterministic
//!   registration-order contract the LSP shipped (issue 042).
//!
//! The registry is data, not behavior: entry points are function
//! pointers into the packs' own reference parsers, so dispatch adds no
//! layer (the `pack_registry` bench pins lookup and dispatch costs).
//! The LSP re-exports these types under `lsp::pack::registry` to keep
//! its public API stable, and the CLI routes `packs`, `check`,
//! `conformance`, and `trace` through the same reference registry.

use std::ops::Range;
use std::sync::Arc;

use runtime_core::events::Event;
use runtime_core::tree::GreenNode;

/// A pack's lossless parse entry point (every shipped pack exposes
/// `parse(&str) -> Result<Arc<GreenNode>, String>`; recovery keeps it
/// lossless, so the `Err` side is structural only).
pub type ParseFn = fn(&str) -> Result<Arc<GreenNode>, String>;

/// A pack's event-stream entry point: `(events, token side table,
/// error count)` — the shape every pack's `reference::parse_events`
/// exposes for tools that walk raw events instead of trees.
pub type ParseEventsFn = fn(&str) -> (Vec<Event>, Vec<(u16, Range<u32>)>, usize);

/// A pack's diagnostic-name resolver: a side-table diagnostic index to
/// the pack's human-readable message (`"syntax error"`, …).
pub type ErrorMessageFn = fn(u32) -> &'static str;

/// A registered pack: identity, editor aliases, and the entry points.
#[derive(Clone, Copy, Debug)]
pub struct PackEntry {
    /// Pack name (the registry's lookup key).
    pub name: &'static str,
    /// The LSP `languageId` the editor sends for this pack.
    pub language_id: &'static str,
    /// File extensions (without the dot) that map to this pack.
    pub extensions: &'static [&'static str],
    /// The lossless reference parser.
    pub parse: ParseFn,
    /// The event-stream parse (see [`ParseEventsFn`]).
    pub parse_events: ParseEventsFn,
    /// The side-table diagnostic namer (see [`ErrorMessageFn`]).
    pub error_message: ErrorMessageFn,
}

/// The pack registry (fixed registration order — deterministic).
#[derive(Debug, Default, Clone)]
pub struct PackRegistry {
    packs: Vec<PackEntry>,
}

impl PackRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a pack. Duplicate names shadow nothing — lookup takes
    /// the first registration — so callers keep one entry per pack.
    pub fn register(&mut self, entry: PackEntry) {
        self.packs.push(entry);
    }

    /// The reference registry: every shipped pack, language order
    /// (flagships first, then the community packs).
    #[must_use]
    pub fn reference() -> Self {
        let mut registry = Self::new();
        let entries: [(
            &'static str,
            &'static str,
            &'static [&'static str],
            ParseFn,
            ParseEventsFn,
            ErrorMessageFn,
        ); 10] = [
            (
                "json",
                "json",
                &["json"][..],
                packs_json::parse,
                packs_json::reference::parse_events,
                packs_json::reference::error_message,
            ),
            (
                "toml",
                "toml",
                &["toml"][..],
                packs_toml::parse,
                packs_toml::reference::parse_events,
                packs_toml::reference::error_message,
            ),
            (
                "python",
                "python",
                &["py"][..],
                packs_python::parse,
                packs_python::reference::parse_events,
                packs_python::reference::error_message,
            ),
            (
                "typescript",
                "typescript",
                &["ts", "tsx", "mts", "cts"][..],
                packs_typescript::parse,
                packs_typescript::reference::parse_events,
                packs_typescript::reference::error_message,
            ),
            (
                "rust",
                "rust",
                &["rs"][..],
                packs_rust::parse,
                packs_rust::reference::parse_events,
                packs_rust::reference::error_message,
            ),
            (
                "c",
                "c",
                &["c", "h"][..],
                packs_c::parse,
                packs_c::reference::parse_events,
                packs_c::reference::error_message,
            ),
            (
                "sql",
                "sql",
                &["sql"][..],
                packs_sql::parse,
                packs_sql::reference::parse_events,
                packs_sql::reference::error_message,
            ),
            (
                "ini",
                "ini",
                &["ini"][..],
                packs_ini::parse,
                packs_ini::reference::parse_events,
                packs_ini::reference::error_message,
            ),
            (
                "csv",
                "csv",
                &["csv"][..],
                packs_csv::parse,
                packs_csv::reference::parse_events,
                packs_csv::reference::error_message,
            ),
            (
                "markdown",
                "markdown",
                &["md", "markdown"][..],
                packs_markdown::parse,
                packs_markdown::reference::parse_events,
                packs_markdown::reference::error_message,
            ),
        ];
        for (name, language_id, extensions, parse, parse_events, error_message) in entries {
            registry.register(PackEntry {
                name,
                language_id,
                extensions,
                parse,
                parse_events,
                error_message,
            });
        }
        registry
    }

    /// Look up a pack by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&PackEntry> {
        self.packs.iter().find(|entry| entry.name == name)
    }

    /// Look up a pack by LSP `languageId` (case-insensitive).
    #[must_use]
    pub fn for_language(&self, language_id: &str) -> Option<&PackEntry> {
        self.packs
            .iter()
            .find(|entry| entry.language_id.eq_ignore_ascii_case(language_id))
    }

    /// Look up a pack by file extension (case-insensitive, leading dot
    /// tolerated).
    #[must_use]
    pub fn for_extension(&self, extension: &str) -> Option<&PackEntry> {
        let extension = extension.trim_start_matches('.');
        self.packs.iter().find(|entry| {
            entry
                .extensions
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
        })
    }

    /// Resolve the pack for a document uri: the uri path's final
    /// extension (plain split — no percent-decoding). `None` when the
    /// uri has no extension (`file:///x/.gitignore` is a dotfile, not
    /// an extension).
    #[must_use]
    pub fn for_uri(&self, uri: &str) -> Option<&PackEntry> {
        let path = uri.split(['?', '#']).next().unwrap_or(uri);
        let file = path.rsplit(['/', '\\']).next().unwrap_or(path);
        let (_, extension) = file.split_once('.')?;
        if extension.is_empty() {
            return None;
        }
        self.for_extension(extension)
    }

    /// The registered entries in registration order.
    #[must_use]
    pub fn entries(&self) -> &[PackEntry] {
        &self.packs
    }

    /// Registered pack names in registration order.
    #[must_use]
    pub fn names(&self) -> Vec<&'static str> {
        self.packs.iter().map(|entry| entry.name).collect()
    }

    /// Number of registered packs.
    #[must_use]
    pub fn len(&self) -> usize {
        self.packs.len()
    }

    /// Whether the registry is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.packs.is_empty()
    }
}

#[cfg(test)]
mod tests;
