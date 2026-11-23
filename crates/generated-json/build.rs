//! build.rs — regenerate the JSON parser crate source on every build (issue 045).
//!
//! Runs the codegen-rust descent backend over the descent-lower JSON
//! fixture and writes the emitted modules into `OUT_DIR`. The crate's
//! `lib.rs` `include!`s `generated_lib.rs`, which in turn declares the
//! `kinds`/`scanner`/`parser` modules (also `include!`d from `OUT_DIR`).
//! ADR-013: regeneration is diff-stable, so identical grammars produce
//! byte-identical files across builds.

use std::io::Write as _;
use std::path::PathBuf;

fn main() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR set"));
    let grammar = gir_syntax::load_grammar(descent_lower::JSON_GRAMMAR).expect("fixture loads");
    let lowered = descent_lower::lower(&grammar).expect("fixture lowers");
    let scanner = lexgen::compile(&grammar).expect("fixture lexes");
    let emitted = codegen_rust::emit_crate(&lowered, &scanner, "json", "1.0.0").expect("emits");

    // Write each module as a standalone file in OUT_DIR. The emitted
    // `lib.rs` body becomes `generated_lib.rs` (the include target); its
    // `pub mod kinds/scanner/parser;` declarations are rewritten to
    // `#[path]`-qualified form so they resolve to the OUT_DIR files
    // (whose `//!` doc comments are then valid as module roots).
    //
    // The emitted lib body opens with a banner + crate-level `//!` docs +
    // `#![forbid(unsafe_code)]`/`#![warn(missing_docs)]` — these belong at
    // the real crate root (provided by this crate's committed `lib.rs`),
    // so strip everything up to (and including) the inner-attribute block.
    let kinds_path = quote(&out_dir.join("kinds.rs"));
    let scanner_path = quote(&out_dir.join("scanner.rs"));
    let parser_path = quote(&out_dir.join("parser.rs"));
    let raw_lib = emitted.file("src/lib.rs").expect("emitted lib.rs present");
    let stripped = strip_crate_header(raw_lib);
    let lib_body = stripped.replace(
        "pub mod kinds;\npub mod parser;\npub mod scanner;\n",
        &format!(
            "#[path = {kinds_path}]\npub mod kinds;\n#[path = {parser_path}]\npub mod parser;\n#[path = {scanner_path}]\npub mod scanner;\n",
        ),
    );
    write_file(&out_dir.join("generated_lib.rs"), &lib_body);
    write_file(
        &out_dir.join("kinds.rs"),
        emitted.file("src/kinds.rs").expect("kinds emitted"),
    );
    write_file(
        &out_dir.join("scanner.rs"),
        emitted.file("src/scanner.rs").expect("scanner emitted"),
    );
    write_file(
        &out_dir.join("parser.rs"),
        emitted.file("src/parser.rs").expect("parser emitted"),
    );

    // Re-run when the emitter or fixture changes.
    println!("cargo:rerun-if-changed=../../crates/codegen-rust/src/descent.rs");
    println!("cargo:rerun-if-changed=../../crates/descent-lower/src/lib.rs");
}

/// Write `contents` to `path`, panicking on IO failure.
fn write_file(path: &std::path::Path, contents: &str) {
    let mut file = std::fs::File::create(path)
        .unwrap_or_else(|err| panic!("create {}: {err}", path.display()));
    file.write_all(contents.as_bytes())
        .unwrap_or_else(|err| panic!("write {}: {err}", path.display()));
}

/// Quote a path for use in `include!("...")`.
fn quote(path: &std::path::Path) -> String {
    format!("\"{}\"", path.display())
}

/// Strip the emitted crate header (banner `//` lines, crate-level `//!`
/// docs, inner attributes `#![...]`, and the blank lines between them)
/// from `lib_body`, returning the remainder that is safe to `include!`
/// after a real crate root. Keeps everything from the first non-header
/// line onward (the `pub mod` declarations and the public API).
fn strip_crate_header(lib_body: &str) -> String {
    let mut out = String::with_capacity(lib_body.len());
    let mut in_header = true;
    for line in lib_body.lines() {
        let trimmed = line.trim_start();
        if in_header {
            // Banner comments, crate-level //! docs, inner attributes
            // (#![...]) and blank lines all belong to the header.
            if trimmed.starts_with("//")
                || trimmed.starts_with("//!")
                || trimmed.starts_with("#![")
                || trimmed.is_empty()
            {
                continue;
            }
            // First real content — stop stripping.
            in_header = false;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}
