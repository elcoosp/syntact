//! cli integration tests (issue 020 TDD): every subcommand driven
//! through [`cli::run`] against real fixture files.

use std::time::{SystemTime, UNIX_EPOCH};

/// The canonical JSON grammar surface (same shape as the gir-syntax
/// fixture).
const JSON_GIR: &str = r#"
language "json" version="1.0.0" extension="json"

tokens {
    number  kind=re "[0-9]+" node="true"
    string  kind=re "\"[^\"\\\\]*\"" node="true" index-bound="\""
    lbrace  kind=lit "{"
    rbrace  kind=lit "}"
    lbrack  kind=lit "["
    rbrack  kind=lit "]"
    colon   kind=lit ":"
    comma   kind=lit ","
    ws      kind=re "[ \\t\\r\\n]+" trivia="true"
}

grammar {
    value      kind=alt object array string number
    object     kind=seq lbrace pair-list rbrace node="true" reuse="true"
    pair-list  kind=sep element=pair separator="," allow-trailing="true"
    pair       kind=seq string colon value
    array      kind=seq lbrack value-list rbrack
    value-list kind=sep element=value separator=","
}
"#;

const GOOD_JSON: &str = "{\n  \"a\": [1, 2],\n  \"b\": \"x\"\n}\n";
const BAD_JSON: &str = "{ \"a\": }";

/// Write `content` to a uniquely named temp file; returns its path.
fn temp_file(name: &str, content: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .subsec_nanos();
    let path =
        std::env::temp_dir().join(format!("syntact-cli-{}-{nanos}-{name}", std::process::id()));
    std::fs::write(&path, content).expect("write fixture");
    path
}

#[test]
fn check_grammar_and_document() {
    let grammar = temp_file("grammar.gir", JSON_GIR);
    let document = temp_file("doc.json", GOOD_JSON);
    assert_eq!(
        cli::run(["check", grammar.to_str().expect("path")]).expect("runs"),
        cli::OK
    );
    assert_eq!(
        cli::run(["check", document.to_str().expect("path")]).expect("runs"),
        cli::OK
    );
    let bad = temp_file("bad.json", BAD_JSON);
    assert_eq!(
        cli::run(["check", bad.to_str().expect("path")]).expect("runs"),
        cli::FAIL
    );
    assert_eq!(
        cli::run(["check", "/no/such/file"]).expect("runs"),
        cli::USAGE
    );
    let _ = std::fs::remove_file(grammar);
    let _ = std::fs::remove_file(document);
    let _ = std::fs::remove_file(bad);
}

#[test]
fn build_reports_pipeline_stages() {
    let grammar = temp_file("build.gir", JSON_GIR);
    let code = cli::run(["build", grammar.to_str().expect("path")]).expect("runs");
    assert_eq!(code, cli::OK);
    let _ = std::fs::remove_file(grammar);
}

#[test]
fn build_with_out_emits_compiling_crate() {
    // The descent-lower JSON fixture (lowers cleanly through the
    // codegen-rust descent backend — issue 045).
    let grammar = temp_file("build_emit.gir", descent_lower::JSON_GRAMMAR);
    let out_dir = std::env::temp_dir().join(format!(
        "syntact-build-out-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&out_dir);
    let code = cli::run([
        "build",
        grammar.to_str().expect("path"),
        "-o",
        out_dir.to_str().expect("path"),
    ])
    .expect("runs");
    assert_eq!(code, cli::OK, "build -o succeeds");
    // The emitted crate has the five files the backend promises.
    for path in [
        "Cargo.toml",
        "src/lib.rs",
        "src/kinds.rs",
        "src/scanner.rs",
        "src/parser.rs",
    ] {
        let file = out_dir.join(path);
        let contents = std::fs::read_to_string(&file)
            .unwrap_or_else(|err| panic!("emitted {path} missing: {err}"));
        assert!(!contents.is_empty(), "emitted {path} is empty");
    }
    // The Cargo.toml carries the safety + lint posture hand-written crates use.
    let toml = std::fs::read_to_string(out_dir.join("Cargo.toml")).expect("Cargo.toml");
    assert!(toml.contains("unsafe_code = \"forbid\""));
    assert!(toml.contains("missing_docs = \"warn\""));
    assert!(toml.contains("runtime-core"));
    assert!(toml.contains("lexgen"));
    let _ = std::fs::remove_dir_all(&out_dir);
    let _ = std::fs::remove_file(grammar);
}

#[test]
fn conflicts_report_deterministic_grammar() {
    let grammar = temp_file("conflicts.gir", JSON_GIR);
    assert_eq!(
        cli::run(["conflicts", grammar.to_str().expect("path")]).expect("runs"),
        cli::OK
    );
    let _ = std::fs::remove_file(grammar);
}

#[test]
fn conformance_gates_byte_faithfulness() {
    let good = temp_file("conf-good.json", GOOD_JSON);
    let bad = temp_file("conf-bad.json", BAD_JSON);
    let both = [
        good.to_str().expect("path").to_owned(),
        bad.to_str().expect("path").to_owned(),
    ];
    assert_eq!(cli::run(["conformance", &both[0]]).expect("runs"), cli::OK);
    assert_eq!(
        cli::run(["conformance", &both[0], &both[1]]).expect("runs"),
        cli::FAIL
    );
    assert_eq!(cli::run(["conformance"]).expect("runs"), cli::USAGE);
    let _ = std::fs::remove_file(good);
    let _ = std::fs::remove_file(bad);
}

#[test]
fn bad_usage_is_an_error_not_a_panic() {
    // Unknown subcommand → clap rejection → Err.
    assert!(cli::run(["frobnicate"]).is_err());
    // No arguments at all → clap rejection → Err.
    let nothing: [String; 0] = [];
    assert!(cli::run(nothing).is_err());
}
