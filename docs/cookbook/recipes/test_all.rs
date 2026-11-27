// Recipe: differential-test the two backends with the test-all oracle.
//
// `syntact test-all` (roadmap §7.4 "Cross-backend oracle", §11.1 #4)
// runs the descent engine and the table-driven LR(1) driver over the
// same scanner feed for every corpus document and gates the agreement:
// the descent verdict (a clean derivation) and the table verdict (the
// LR accept action) must match — "our own backends keep each other
// honest". This recipe drives the library seam (`cli::testall`).

use cli::testall;

#[test]
fn differential_test_the_backends() {
    // The corpus: valid documents (the fixture start rule is `value*`,
    // so multi-value documents are valid) and malformed documents that
    // both backends must reject in agreement.
    let docs = [
        ("flat.json", "{\"name\": \"syntact\", \"n\": 42}"),
        ("multi.json", "1 2"),
        ("missing_value.json", "{\"a\": }"),
        ("unclosed_array.json", "[1, 2"),
    ];
    let corpus: Vec<testall::CorpusDoc> = docs
        .iter()
        .map(|(name, source)| testall::CorpusDoc {
            name: (*name).to_string(),
            source: (*source).to_string(),
        })
        .collect();

    // Run the oracle over the JSON fixture grammar (pure — no
    // filesystem; named refusals for a grammar that does not load or
    // lower, a lexer refusal, or an empty corpus).
    let report = testall::run(descent_lower::JSON_GRAMMAR, &corpus).expect("the fixture runs");

    // Every document agrees, and an independent second evaluation
    // (fresh loader → automaton → feeds) reproduced the verdicts.
    assert_eq!(report.disagreements, 0);
    assert!(report.deterministic);
    assert_eq!(report.documents.len(), 4);

    // The rejected documents carry the table backend's step error:
    // state, offending byte offset, and the expected terminal set by
    // token name (the §7.4 expected-set extraction).
    let rejected = &report.documents[2];
    assert!(!rejected.descent_valid);
    let error = rejected.lr_error.as_ref().expect("table rejection");
    assert!(!error.expected.is_empty());

    // The machine-readable report is byte-stable for identical inputs
    // (ADR-013), so tooling can diff two oracle runs.
    assert_eq!(testall::render_json(&report), testall::render_json(&report));
}
