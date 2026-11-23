//! pack-registry test suite (issue 046 TDD): the reference registration,
//! the three lookup axes, and a per-pack smoke proving every entry's
//! parser and event stream really work through the seam.

use std::sync::Arc;

use crate::{PackEntry, PackRegistry};
use runtime_core::events::Event;
use runtime_core::tree::GreenNode;

/// Minimal valid source per pack (0 tolerated errors, taken from each
/// pack's own suite).
const SNIPPETS: &[(&str, &str)] = &[
    ("json", "{\"a\": 1}"),
    ("toml", "key = 1\n"),
    ("python", "x = 1"),
    ("typescript", "export const x = 1;"),
    ("rust", "fn f() { let x = 1 + 2 * 3; }"),
    ("c", "int (*f)(void);"),
    ("sql", "SELECT * FROM users;"),
    ("ini", "[section]\na = 1\nb = 2"),
    ("csv", "name,age\nalice,42\n"),
    ("markdown", "# Title\n"),
];

/// Minimal malformed source per pack (≥ 1 tolerated error, taken from
/// each pack's own suite) — the smoke for `error_message` resolution.
const BROKEN: &[(&str, &str)] = &[
    ("json", "{ \"a\": }"),
    ("toml", "key = [1, \nother = \n"),
    ("python", "x = @"),
    ("typescript", "let x = ;"),
    ("rust", "fn f() { let x = 1 + ; }"),
    ("c", "int (*f)(;"),
    ("sql", "SELECT FROM;"),
    ("ini", "= 1"),
    ("csv", "a,\"broken"),
    ("markdown", "#\r"),
];

#[test]
fn reference_registers_every_shipped_pack_in_order() {
    let registry = PackRegistry::reference();
    assert_eq!(registry.len(), 10);
    assert!(!registry.is_empty());
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
fn entries_expose_identity_and_extensions_in_order() {
    let registry = PackRegistry::reference();
    let entries = registry.entries();
    assert_eq!(entries.len(), 10);
    assert_eq!(entries[0].name, "json");
    assert_eq!(entries[0].language_id, "json");
    assert_eq!(entries[0].extensions, &["json"][..]);
    assert_eq!(entries[3].name, "typescript");
    assert_eq!(entries[3].extensions, &["ts", "tsx", "mts", "cts"][..]);
    assert_eq!(entries[5].name, "c");
    assert_eq!(entries[5].extensions, &["c", "h"][..]);
}

#[test]
fn lookups_cover_name_language_and_extension() {
    let registry = PackRegistry::reference();
    assert_eq!(registry.get("json").expect("json").name, "json");
    assert_eq!(registry.get("missing").map(|entry| entry.name), None);
    assert_eq!(
        registry.for_language("Python").expect("python").name,
        "python"
    );
    assert_eq!(
        registry.for_language("shellscript").map(|entry| entry.name),
        None
    );
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
    assert_eq!(
        registry
            .for_uri("file:///C:/src/main.rs?raw")
            .expect("rs")
            .name,
        "rust"
    );
    assert!(registry.for_uri("file:///etc/hosts").is_none());
    assert!(registry.for_uri("file:///repo/.gitignore").is_none());
    assert!(registry.for_uri("file:///tmp/x.").is_none());
    assert!(registry.for_uri("file:///dir/").is_none());
}

#[test]
fn every_entry_parses_renders_and_streams_events() {
    let registry = PackRegistry::reference();
    for (name, source) in SNIPPETS {
        let entry = registry.get(name).expect("registered");
        let tree: Arc<GreenNode> = (entry.parse)(source).expect("parses");
        let input = runtime_core::input::Source::from_str_unchecked(source);
        assert_eq!(tree.render(&input), *source, "{name} must be byte-faithful");

        let (events, ranges, errors) = (entry.parse_events)(source);
        assert_eq!(errors, 0, "{name} must parse clean");
        assert!(!events.is_empty(), "{name} must emit events");
        let tokens = events
            .iter()
            .filter(|event| matches!(event, Event::Token { .. }))
            .count();
        assert_eq!(tokens, ranges.len(), "{name} token/range parallelism");
    }
}

#[test]
fn every_entry_reports_errors_with_named_diagnostics() {
    let registry = PackRegistry::reference();
    for (name, source) in BROKEN {
        let entry = registry.get(name).expect("registered");
        let (events, _ranges, errors) = (entry.parse_events)(source);
        assert!(errors > 0, "{name} must flag `{source}`");
        let diags: Vec<u32> = events
            .iter()
            .filter_map(|event| match event {
                Event::Error { diag } => Some(diag),
                _ => None,
            })
            .copied()
            .collect();
        assert!(!diags.is_empty(), "{name} must emit Error events");
        for diag in diags {
            let message = (entry.error_message)(diag);
            assert!(!message.is_empty(), "{name} diag {diag} must name itself");
        }
    }
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
        parse_events: packs_json::reference::parse_events,
        error_message: packs_json::reference::error_message,
    });
    assert_eq!(registry.len(), 1);
    assert_eq!(registry.get("json").expect("registered").name, "json");
    assert!(registry.get("toml").is_none());
}
