//! The pack registry (issue 042): name, language id, extensions, and the
//! pack's `parse` entry point — the same deterministic-registration shape
//! as the playground server's `Registry`, resolved per document by
//! `languageId` first and uri extension second.

use std::sync::Arc;

use runtime_core::tree::GreenNode;

/// A pack's parse entry point (every shipped pack exposes
/// `parse(&str) -> Result<Arc<GreenNode>, String>`; recovery keeps it
/// lossless, so the `Err` side is structural only).
pub type ParseFn = fn(&str) -> Result<Arc<GreenNode>, String>;

/// A registered pack: identity, editor aliases, and the parser.
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

    /// Register a pack.
    pub fn register(&mut self, entry: PackEntry) {
        self.packs.push(entry);
    }

    /// The reference registry: every shipped pack, language order
    /// (flagships first, then the community packs).
    #[must_use]
    pub fn reference() -> Self {
        let mut registry = Self::new();
        let entries = [
            ("json", "json", &["json"][..], packs_json::parse as ParseFn),
            ("toml", "toml", &["toml"][..], packs_toml::parse as ParseFn),
            (
                "python",
                "python",
                &["py"][..],
                packs_python::parse as ParseFn,
            ),
            (
                "typescript",
                "typescript",
                &["ts", "tsx", "mts", "cts"][..],
                packs_typescript::parse as ParseFn,
            ),
            ("rust", "rust", &["rs"][..], packs_rust::parse as ParseFn),
            ("c", "c", &["c", "h"][..], packs_c::parse as ParseFn),
            ("sql", "sql", &["sql"][..], packs_sql::parse as ParseFn),
            ("ini", "ini", &["ini"][..], packs_ini::parse as ParseFn),
            ("csv", "csv", &["csv"][..], packs_csv::parse as ParseFn),
            (
                "markdown",
                "markdown",
                &["md", "markdown"][..],
                packs_markdown::parse as ParseFn,
            ),
        ];
        for (name, language_id, extensions, parse) in entries {
            registry.register(PackEntry {
                name,
                language_id,
                extensions,
                parse,
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

    /// Look up a pack by file extension (case-insensitive).
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
mod tests {
    use super::*;

    #[test]
    fn reference_registers_every_shipped_pack_in_order() {
        let registry = PackRegistry::reference();
        assert_eq!(registry.len(), 10);
        assert_eq!(
            registry.names(),
            [
                "json",
                "toml",
                "python",
                "typescript",
                "rust",
                "c",
                "sql",
                "ini",
                "csv",
                "markdown"
            ]
        );
    }

    #[test]
    fn language_lookup_is_case_insensitive() {
        let registry = PackRegistry::reference();
        assert_eq!(registry.for_language("json").expect("json").name, "json");
        assert_eq!(
            registry.for_language("Python").expect("python").name,
            "python"
        );
        assert_eq!(
            registry.for_language("shellscript").map(|entry| entry.name),
            None
        );
    }

    #[test]
    fn extension_lookup_is_case_insensitive_and_dot_tolerant() {
        let registry = PackRegistry::reference();
        assert_eq!(registry.for_extension("json").expect("json").name, "json");
        assert_eq!(registry.for_extension(".PY").expect("py").name, "python");
        assert_eq!(registry.for_extension("TS").expect("ts").name, "typescript");
        assert_eq!(
            registry.for_extension("mts").expect("mts").name,
            "typescript"
        );
        assert_eq!(registry.for_extension("h").expect("h").name, "c");
        assert!(registry.for_extension("gir").is_none());
        assert!(registry.for_extension("").is_none());
    }

    #[test]
    fn uri_lookup_resolves_the_final_path_extension() {
        let registry = PackRegistry::reference();
        assert_eq!(
            registry
                .for_uri("file:///home/u/config.toml")
                .expect("toml")
                .name,
            "toml"
        );
        assert_eq!(
            registry.for_uri("untitled:notes.MD").expect("md").name,
            "markdown"
        );
        // Windows separators and query strings are handled.
        assert_eq!(
            registry
                .for_uri("file:///C:/src/main.rs?raw")
                .expect("rs")
                .name,
            "rust"
        );
        // No extension, dotfile, and trailing-dot cases do not resolve.
        assert!(registry.for_uri("file:///etc/hosts").is_none());
        assert!(registry.for_uri("file:///repo/.gitignore").is_none());
        assert!(registry.for_uri("file:///tmp/x.").is_none());
        assert!(registry.for_uri("file:///dir/").is_none());
    }

    #[test]
    fn custom_registries_register_in_order() {
        let mut registry = PackRegistry::new();
        assert!(registry.is_empty());
        registry.register(PackEntry {
            name: "json",
            language_id: "json",
            extensions: &["json"],
            parse: packs_json::parse,
        });
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.get("json").expect("registered").name, "json");
        assert!(registry.get("toml").is_none());
    }
}
