//! Example: run the cross-backend differential oracle over the JSON
//! fixture.
//!
//! `syntact test-all` (roadmap §7.4 "Cross-backend oracle", §11.1 #4)
//! differential-tests the descent engine against the table-driven LR(1)
//! driver over a corpus through one grammar — the two backends must
//! agree on every document ("our own backends keep each other honest").
//! This example drives the library seam (`cli::testall`) over a small
//! fixture corpus and prints each document's two verdicts.
//!
//! Run: `cargo run -p cli --example testall`

fn main() {
    // The corpus: a valid document, a multi-value document (the fixture
    // grammar's start rule is `value*`), and two malformed documents —
    // both backends must reject those in agreement.
    let docs = [
        ("flat.json", "{\"name\": \"syntact\", \"n\": 42}"),
        ("multi.json", "1 2"),
        ("missing_value.json", "{\"a\": }"),
        ("unclosed_array.json", "[1, 2"),
    ];

    let corpus: Vec<cli::testall::CorpusDoc> = docs
        .iter()
        .map(|(name, source)| cli::testall::CorpusDoc {
            name: (*name).to_string(),
            source: (*source).to_string(),
        })
        .collect();

    // Run the oracle (pure; named refusals) — the JSON fixture grammar
    // ships with `descent-lower`.
    let report = cli::testall::run(descent_lower::JSON_GRAMMAR, &corpus).expect("the fixture runs");

    println!(
        "grammar `{}`: descent vs table (LR(1)) over {} document(s)",
        report.grammar,
        report.documents.len()
    );
    for document in &report.documents {
        println!(
            "  {:<22} descent(errors={}) table(accepted={}) -> {}",
            document.name,
            document.descent_errors,
            document.lr_accepted,
            if document.agree { "agree" } else { "DISAGREE" }
        );
    }
    println!(
        "verdict: {}/{} agree, deterministic={}",
        report.agreements,
        report.documents.len(),
        report.deterministic
    );

    // The agreement is the gate: a disagreement means one backend
    // mis-classifies the language the other implements.
    assert!(report.disagreements == 0 && report.deterministic);
    println!("the backends keep each other honest");
}
