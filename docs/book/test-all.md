# The cross-backend oracle

The roadmap makes a promise about its own backends (§7.4 "Cross-backend
oracle"): *"`syntact test-all` emits descent and table backends together
and differential-tests them on the corpus (lalrpop `TestAll` pattern) —
our own backends keep each other honest."* Two implementations of one
grammar can drift apart silently; a differential oracle is the mechanism
that makes drift a test failure instead of a surprise. This chapter is
the command that keeps the promise, plus the runtime it needed: the
table-driven LR(1) driver (`lr_shadow::driver`), the O1 tier of the
§7.10 optimization table — until this seam, `codegen-rust`'s table
emission had no runtime consumer, and the two backends had nothing that
compared them.

## Two backends, one feed

Both backends consume the same scanner feed — one `Lowered::feed` per
document, the same non-trivia token stream the engine runs on:

- **descent** — the descriptor interpreter (`descent::parse_events`).
  Its verdict is a clean derivation: zero `Event::Error`s over a
  non-empty feed. (The engine closes a zero-progress start silently, so
  an empty feed counts as accepted exactly when the start rule is
  nullable — the oracle reads that nullability from the lowering, and
  the table backend's ε derivation agrees by construction.)
- **table** — the LR(1) stack machine over the shadow automaton's
  `action`/`goto` tables (`lr_shadow::driver::parse`). Its verdict is
  the accept action: the whole feed derived down to the pseudo-EOF.

A document **agrees** when the two verdicts match. A rejected run also
carries the table backend's step error — the error state, the offending
byte offset, and the state's expected terminal set rendered by token
name (the §7.4 expected-set extraction, now consumed by diagnostics
instead of staying analysis-only). The oracle runs everything twice —
fresh loader, fresh automaton, fresh feeds — and refuses to call itself
deterministic unless the second evaluation reproduces the first byte
for byte, so the agreement verdict is stable across runs, machines, and
CI.

```rust
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
```
The command form is `syntact test-all <GRAMMAR.gir> <FILES…>`: any
grammar the loader accepts, any corpus of documents (`-` reads stdin).
The exit code is the verdict — 0 when every document agrees on a
deterministic run, 1 when a backend disagrees (one implementation is
wrong about the language), 2 for usage and read errors. `--json` renders
the report as byte-stable JSON (ADR-013), documents in input order, so
tooling can diff two oracle runs.
