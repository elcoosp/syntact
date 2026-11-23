//! The pack registry (issue 042): name, language id, extensions, and the
//! pack's `parse` entry point — resolved per document by `languageId`
//! first and uri extension second.
//!
//! Since issue 046 the registration itself lives in the shared
//! [`pack_registry`] crate: one canonical roster (all ten shipped packs,
//! deterministic language order) consumed unchanged by the LSP, the CLI
//! (`packs`/`check`/`conformance`/`trace`), and the playground server.
//! This module re-exports the seam under its original public paths so
//! the issue-042 API surface stays stable.

pub use pack_registry::{PackEntry, PackRegistry, ParseFn};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_registry_is_the_shared_ten_pack_roster() {
        // The re-exported registry is the same deterministic registration
        // issue 042 shipped — now sourced from the shared crate.
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
    fn language_and_uri_lookups_flow_through_the_reexport() {
        let registry = PackRegistry::reference();
        assert_eq!(
            registry.for_language("Python").expect("python").name,
            "python"
        );
        assert_eq!(
            registry
                .for_uri("file:///home/u/config.toml")
                .expect("toml")
                .name,
            "toml"
        );
        assert!(registry.for_uri("file:///repo/.gitignore").is_none());
    }

    #[test]
    fn custom_registries_register_through_the_reexport() {
        let mut registry = PackRegistry::new();
        assert!(registry.is_empty());
        registry.register(PackEntry {
            name: "json",
            language_id: "json",
            extensions: &["json"],
            parse: packs_json::parse,
            parse_events: packs_json::reference::parse_events,
            error_message: packs_json::reference::error_message,
        });
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.get("json").expect("registered").name, "json");
        assert!(registry.get("toml").is_none());
    }
}
