//! `syntact import` gates (roadmap §9.1 command list, ADR-015): the
//! tree-sitter adoption wedge as a CLI capability. The acceptance
//! criterion pinned by the first test: `syntact import <grammar.js>
//! -o <file>` converts a tree-sitter `grammar.js` into gir-surface KDL
//! through the importer (issue 037), the converted surface loads back
//! through `gir-syntax` (an import nobody can load is worthless), and
//! the conversion report says what happened to every construct.

use cli::import::{self, ImportCommandError};

/// A uniquely named temp path for this process.
fn temp_path(name: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "syntact-cli-import-{}-{nanos}-{name}",
        std::process::id()
    ))
}

/// A tree-sitter `grammar.js` fixture (the JSON shape of the importer
/// conformance wedge, issue 037): choice, seq, repeat-separated pairs,
/// optional lists, field annotations, a `token()`-wrapped regex, extras.
const JSON_GRAMMAR_JS: &str = r#"
module.exports = grammar({
  name: 'json',
  extras: $ => [/\s/, $.comment],
  rules: {
    value: $ => choice($.object, $.array, $.string, $.number),
    object: $ => seq('{', repeat(seq($.pair, ',')), '}'),
    pair: $ => seq(field('key', $.string), ':', field('value', $.value)),
    array: $ => seq('[', optional(seq($.value, repeat(seq(',', $.value)))), ']'),
    string: $ => /"[^"]*"/,
    number: $ => /-?[0-9]+(\.[0-9]+)?([eE][+-]?[0-9]+)?/,
    comment: $ => token(/#[^\r\n]*/),
  }
});
"#;

#[test]
fn import_converts_grammar_js_to_a_loadable_gir_grammar() {
    // Roadmap §9.1: `syntact import tree-sitter ./grammar.js -o mylang.gir`.
    // ADR-015: the importer is the adoption wedge — the conversion lands
    // on disk and loads back through the pipeline.
    let source = temp_path("grammar.js");
    std::fs::write(&source, JSON_GRAMMAR_JS).expect("write fixture");
    let out = temp_path("imported.gir");
    let exit = cli::run([
        "import",
        "-o",
        out.to_str().expect("path"),
        source.to_str().expect("path"),
    ])
    .expect("runs");
    assert_eq!(exit, cli::OK);
    let kdl = std::fs::read_to_string(&out).expect("written grammar");
    let grammar = gir_syntax::load_grammar(&kdl).expect("the converted surface loads");
    assert_eq!(grammar.name, "json");
    assert_ne!(kdl.trim(), "");
}

#[test]
fn import_reports_what_happened_to_every_construct() {
    // ADR-015's provenance log: mapped/dropped/why, in traversal order.
    let source = temp_path("report.js");
    std::fs::write(&source, JSON_GRAMMAR_JS).expect("write fixture");
    let out = temp_path("report.gir");
    let exit = cli::run([
        "import",
        "-o",
        out.to_str().expect("path"),
        source.to_str().expect("path"),
    ])
    .expect("runs");
    assert_eq!(exit, cli::OK);
    // The imported JSON grammar maps its rules and carries the extras
    // through as trivia tokens with a provenance entry each; nothing
    // is dropped silently.
    let imported = import::import_document(JSON_GRAMMAR_JS).expect("imports");
    assert!(
        imported
            .report
            .has(importer::Action::Mapped, "rule", "object"),
        "the object rule is mapped"
    );
    assert!(
        imported
            .report
            .has(importer::Action::Mapped, "extras", "extra1"),
        "the whitespace extra is mapped to a trivia token"
    );
    assert!(
        imported
            .report
            .entries
            .iter()
            .any(|entry| entry.construct == "extras" && entry.detail.contains("comment")),
        "the comment extra names its token"
    );
    assert_eq!(imported.report.dropped(), 0, "nothing dropped here");
}

#[test]
fn import_is_deterministic() {
    // ADR-013 posture: same input → same output, twice.
    let source = temp_path("det.js");
    std::fs::write(&source, JSON_GRAMMAR_JS).expect("write fixture");
    let first = temp_path("det1.gir");
    let second = temp_path("det2.gir");
    for out in [&first, &second] {
        let exit = cli::run([
            "import",
            "-o",
            out.to_str().expect("path"),
            source.to_str().expect("path"),
        ])
        .expect("runs");
        assert_eq!(exit, cli::OK);
    }
    assert_eq!(
        std::fs::read_to_string(&first).expect("first"),
        std::fs::read_to_string(&second).expect("second")
    );
}

#[test]
fn import_json_report_is_stable() {
    let imported = import::import_document(JSON_GRAMMAR_JS).expect("imports");
    let report = import::render_json(&imported.report);
    assert!(report.contains("\"entries\": ["), "{report}");
    assert!(report.contains("\"action\": \"mapped\""), "{report}");
    assert!(report.contains("\"dropped\": "), "{report}");
    let again = import::render_json(
        &import::import_document(JSON_GRAMMAR_JS)
            .expect("imports")
            .report,
    );
    assert_eq!(report, again);
}

#[test]
fn import_stdout_mode_exits_ok() {
    let source = temp_path("stdout.js");
    std::fs::write(&source, JSON_GRAMMAR_JS).expect("write fixture");
    let exit = cli::run(["import", source.to_str().expect("path")]).expect("runs");
    assert_eq!(exit, cli::OK);
}

#[test]
fn import_refuses_malformed_sources_and_missing_files() {
    // No grammar statement → named import refusal → FAIL.
    let bad = temp_path("bad.js");
    std::fs::write(&bad, "this is not a grammar").expect("write fixture");
    assert_eq!(
        cli::run(["import", bad.to_str().expect("path")]).expect("runs"),
        cli::FAIL
    );
    let error = import::import_document("this is not a grammar").expect_err("refused");
    match &error {
        ImportCommandError::Import(importer::ImportError::NoGrammar) => {}
        other => panic!("unexpected error: {other:?}"),
    }
    // Unreadable file → usage error.
    assert_eq!(
        cli::run(["import", "/no/such/grammar.js"]).expect("runs"),
        cli::USAGE
    );
}
