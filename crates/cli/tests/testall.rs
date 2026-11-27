//! The cross-backend differential oracle gate (roadmap §7.4, §11.1 #4).
//!
//! Plan line pinned (§7.4 "Cross-backend oracle"): *"`syntact test-all`
//! emits descent and table backends together and differential-tests them
//! on the corpus (lalrpop `TestAll` pattern) — our own backends keep each
//! other honest."*
//!
//! The oracle contract, per corpus document: the descent engine accepts
//! (zero syntax errors, the whole feed derived — or a nullable start over
//! an empty feed) if and only if the table-driven LR(1) driver accepts
//! the same scanner feed. The nullable-start clause is pinned below: the
//! engine closes a zero-progress start silently, so an empty feed counts
//! as accepted exactly when the start rule is nullable — the LR side
//! agrees by construction (its ε derivation).

use cli::testall::{self, CorpusDoc};

/// The JSON fixture corpus. Valid documents parse with zero syntax
/// errors on the descent engine (`document` is `value*`, so multi-value
/// and empty documents are valid); malformed documents produce recovery
/// regions on the engine and a rejection on the table backend.
const VALID: &[(&str, &str)] = &[
    ("object.json", "{}"),
    ("array.json", "[]"),
    ("pair.json", r#"{"a": 1}"#),
    (
        "nested.json",
        r#"{"name": "syntact", "version": 1, "ok": true, "bits": [true, false, null]}"#,
    ),
    ("spaced.json", "  [ 1 , 2 ]  "),
    ("multi_value.json", "1 2"),
    ("empty.json", ""),
    ("trivia_only.json", "  \n\t "),
];

const MALFORMED: &[(&str, &str)] = &[
    ("missing_value.json", "{\"a\": }"),
    ("unclosed_array.json", "[1, 2"),
    ("garbage.json", "@#$"),
    ("unclosed_object.json", "{\"a\": 1"),
    ("stray_closer.json", "}"),
    ("half_keyword.json", "tru"),
];

/// A grammar whose start rule is NOT nullable (`document` is one
/// required value): the empty document must reject on both backends.
const REQUIRED_GIR: &str = r#"
language "req" version="1.0.0" extension="req"

tokens {
    a kind=lit "a"
}

grammar {
    document kind=seq value
    value    kind=alt atom
    atom     kind=seq a
}
"#;

fn corpus(entries: &[(&str, &str)]) -> Vec<CorpusDoc> {
    entries
        .iter()
        .map(|(name, source)| CorpusDoc {
            name: (*name).to_string(),
            source: (*source).to_string(),
        })
        .collect()
}

#[test]
fn the_backends_agree_on_every_corpus_document() {
    let mut docs = corpus(VALID);
    docs.extend(corpus(MALFORMED));
    let report = testall::run(descent_lower::JSON_GRAMMAR, &docs).expect("the fixture runs");

    assert_eq!(report.documents.len(), docs.len());
    assert!(report.deterministic, "an independent second run agrees");
    for document in &report.documents {
        assert!(
            document.agree,
            "{}: descent (errors={}) vs table (accepted={}) disagree",
            document.name, document.descent_errors, document.lr_accepted
        );
    }
    assert_eq!(report.disagreements, 0);
    assert_eq!(report.agreements, docs.len());
}

#[test]
fn valid_documents_accept_on_both_backends() {
    let report = testall::run(descent_lower::JSON_GRAMMAR, &corpus(VALID)).expect("runs");
    for document in &report.documents {
        assert!(
            document.descent_valid,
            "{}: engine parses clean",
            document.name
        );
        assert!(
            document.lr_accepted,
            "{}: table backend accepts",
            document.name
        );
        assert!(document.lr_error.is_none());
    }
    // The multi-value document really consumed both values.
    let multi = report
        .documents
        .iter()
        .find(|document| document.name == "multi_value.json")
        .expect("present");
    assert_eq!(multi.tokens, 2, "the feed holds both number tokens");
}

#[test]
fn malformed_documents_reject_on_both_backends() {
    let report = testall::run(descent_lower::JSON_GRAMMAR, &corpus(MALFORMED)).expect("runs");
    for document in &report.documents {
        assert!(
            !document.descent_valid,
            "{}: engine reports damage",
            document.name
        );
        assert!(
            !document.lr_accepted,
            "{}: table backend rejects",
            document.name
        );
        let error = document.lr_error.as_ref().expect("LR step error reported");
        assert!(error.state < u32::MAX, "the error state is a real state id");
        assert!(!error.expected.is_empty(), "the expected set is reported");
        assert!(
            error.offset <= document.bytes as u32,
            "the error sits inside the document"
        );
    }
}

#[test]
fn the_nullable_start_accepts_the_empty_document_on_both_backends() {
    // `document` is `value*` (the backward-binding `rep` marker): the
    // engine closes the zero-progress start silently with zero errors
    // over an empty feed, and the table backend accepts the ε
    // derivation. The verdict rule's nullable clause pins this exact
    // agreement — a non-nullable start rejects instead (next test).
    let report = testall::run(
        descent_lower::JSON_GRAMMAR,
        &corpus(&[("empty.json", ""), ("trivia_only.json", "  \n\t ")]),
    )
    .expect("runs");
    for document in &report.documents {
        assert!(
            document.descent_valid,
            "{}: nullable start accepts",
            document.name
        );
        assert!(
            document.lr_accepted,
            "{}: ε derivation accepts",
            document.name
        );
        assert!(document.agree);
    }
}

#[test]
fn the_non_nullable_start_rejects_the_empty_document_on_both_backends() {
    let report = testall::run(
        REQUIRED_GIR,
        &corpus(&[("empty.req", ""), ("one.req", "a")]),
    )
    .expect("runs");
    let empty = &report.documents[0];
    assert!(!empty.descent_valid, "a required value is missing");
    assert!(!empty.lr_accepted, "the table backend rejects too");
    assert!(empty.agree);
    let one = &report.documents[1];
    assert!(one.descent_valid && one.lr_accepted && one.agree);
}

#[test]
fn rejected_runs_report_where_and_what() {
    // `[1, 2` — the table backend dies at the pseudo-EOF after `2`, and
    // the report renders the expected terminals by name.
    let report = testall::run(
        descent_lower::JSON_GRAMMAR,
        &corpus(&[("unclosed_array.json", "[1, 2")]),
    )
    .expect("runs");
    let document = &report.documents[0];
    let error = document.lr_error.as_ref().expect("rejection");
    // `Automaton::expected` order: tokens by id, eof last — the fixture
    // declares `rbrack` before `comma`.
    assert_eq!(
        error.expected,
        vec!["rbrack".to_string(), "comma".to_string()]
    );
}

#[test]
fn reports_are_deterministic_across_runs() {
    let mut docs = corpus(VALID);
    docs.extend(corpus(MALFORMED));
    let first = testall::run(descent_lower::JSON_GRAMMAR, &docs).expect("runs");
    let second = testall::run(descent_lower::JSON_GRAMMAR, &docs).expect("runs");
    assert_eq!(first, second);
    assert!(first.deterministic);
}

#[test]
fn json_rendering_is_byte_stable_and_names_the_fields() {
    let report = testall::run(descent_lower::JSON_GRAMMAR, &corpus(VALID)).expect("runs");
    let first = testall::render_json(&report);
    let second = testall::render_json(&report);
    assert_eq!(first, second);
    for needle in [
        "\"grammar\": \"json\"",
        "\"documents\"",
        "\"descent_errors\"",
        "\"lr_accepted\"",
        "\"agree\": true",
        "\"deterministic\": true",
    ] {
        assert!(first.contains(needle), "json lacks `{needle}`:\n{first}");
    }
}

#[test]
fn refused_inputs_are_named() {
    // An unparsable grammar refuses with the loader's diagnostic…
    let error =
        testall::run("language { broken", &corpus(VALID)).expect_err("a broken grammar refuses");
    assert!(matches!(error, testall::TestAllError::Grammar(_)));
    // …an empty corpus refuses before anything runs.
    let error = testall::run(descent_lower::JSON_GRAMMAR, &[]).expect_err("no corpus");
    assert!(matches!(error, testall::TestAllError::EmptyCorpus));
}

#[test]
fn the_command_wires_the_seam_end_to_end() {
    let grammar = temp_file("testall.gir", descent_lower::JSON_GRAMMAR);
    let good = temp_file("testall_ok.json", "{\"a\": [1, true, null]}");
    let bad = temp_file("testall_bad.json", "{\"a\": }");
    let code = cli::run([
        "test-all",
        grammar.to_str().expect("utf8"),
        good.to_str().expect("utf8"),
        bad.to_str().expect("utf8"),
    ])
    .expect("parses");
    assert_eq!(code, cli::OK, "agreement is the OK verdict");
    // Usage refusals: no corpus documents.
    let code = cli::run(["test-all", grammar.to_str().expect("utf8")]).expect("parses");
    assert_eq!(code, cli::USAGE);
    // An unreadable grammar file is a usage error.
    let code = cli::run([
        "test-all",
        "/nonexistent/syntact.gir",
        &good.to_string_lossy(),
    ])
    .expect("parses");
    assert_eq!(code, cli::USAGE);
}

/// Write `content` to a uniquely named temp file; returns its path (the
/// `cli.rs` convention).
fn temp_file(name: &str, content: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "syntact-testall-{}-{nanos}-{name}",
        std::process::id()
    ));
    std::fs::write(&path, content).expect("write temp file");
    path
}
