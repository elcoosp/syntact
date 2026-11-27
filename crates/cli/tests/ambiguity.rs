//! The CLI ambiguity-report gate (roadmap §6.1 pass 4 output artifacts,
//! §6.2, ADR-007).
//!
//! Plan line pinned (§6.2): *"requires `@allow_ambiguous` or gets an error
//! with two counter-example derivations"* — `syntact conflicts` fails on
//! unannotated ambiguous regions (the report renders both derivations),
//! and an annotated region is accepted as a GLR island candidate so the
//! build continues.

use std::path::PathBuf;

/// Writes a fixture and returns its path (cleaned up by the caller).
fn temp_file(name: &str, contents: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("syntact_ambiguity_{name}"));
    std::fs::write(&path, contents).expect("fixture writes");
    path
}

const AMBIGUOUS_GIR: &str = r#"
language "amb" version="1.0.0"

tokens {
    a kind=lit "a"
}

grammar {
    s kind=alt p q
    p kind=seq a
    q kind=seq a
}
"#;

const ANNOTATED_GIR: &str = r#"
language "amb" version="1.0.0"

tokens {
    a kind=lit "a"
}

grammar {
    s kind=alt p q allow-ambiguous="true"
    p kind=seq a
    q kind=seq a
}
"#;

#[test]
fn unannotated_ambiguity_fails_the_conflicts_gate() {
    let grammar = temp_file("amb_plain.gir", AMBIGUOUS_GIR);
    assert_eq!(
        cli::run(["conflicts", grammar.to_str().expect("path")]).expect("runs"),
        cli::FAIL,
        "unannotated ambiguous regions get the error-grade verdict"
    );
    let _ = std::fs::remove_file(grammar);
}

#[test]
fn annotated_ambiguity_is_accepted_as_an_island_candidate() {
    let grammar = temp_file("amb_island.gir", ANNOTATED_GIR);
    assert_eq!(
        cli::run(["conflicts", grammar.to_str().expect("path")]).expect("runs"),
        cli::OK,
        "the build continues with the GLR island candidate"
    );
    assert_eq!(
        cli::run(["build", grammar.to_str().expect("path")]).expect("runs"),
        cli::OK,
        "the front-half summary accepts the annotated grammar"
    );
    let _ = std::fs::remove_file(grammar);
}
