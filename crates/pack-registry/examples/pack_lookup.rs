//! The pack-registry seam from a consumer's point of view: list the
//! registered packs, resolve one the way an editor does (language id)
//! and one the way a file watcher does (extension), then parse through
//! the resolved entries and prove the results are byte-faithful.
//!
//! Run: `cargo run -p pack-registry --example pack_lookup`

use std::sync::Arc;

use pack_registry::PackRegistry;
use runtime_core::tree::GreenNode;

fn main() {
    let registry = PackRegistry::reference();
    println!("registered packs ({}):", registry.len());
    for entry in registry.entries() {
        println!(
            "  {:<10} language `{:<10}` extensions: {}",
            entry.name,
            entry.language_id,
            entry
                .extensions
                .iter()
                .map(|extension| format!(".{extension}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    // Editor-style resolution: the language id is case-insensitive.
    let python = registry
        .for_language("Python")
        .expect("python is registered");
    let python_doc = "x = 1";
    parse_and_render(python.name, python.parse, python_doc);

    // File-watcher-style resolution: extension, dot- and case-tolerant.
    let toml = registry.for_extension(".TOML").expect("toml is registered");
    let toml_doc = "key = 1\n";
    parse_and_render(toml.name, toml.parse, toml_doc);

    // Unknown lookups resolve to None — a tool surfaces that instead of
    // guessing a parser.
    assert!(registry.for_extension("gir").is_none());
    println!("unknown extensions resolve to None (no guessed parser)");
}

/// Parse through a registry entry and prove byte-faithfulness.
fn parse_and_render(name: &str, parse: fn(&str) -> Result<Arc<GreenNode>, String>, source: &str) {
    let tree = parse(source).expect("pack parses its own snippet");
    let input = runtime_core::input::Source::from_str_unchecked(source);
    assert_eq!(tree.render(&input), source, "{name} must be byte-faithful");
    println!("  {name}: parsed {} bytes, render == source", source.len());
}
