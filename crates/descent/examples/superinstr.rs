//! Example: PGO-driven superinstruction mining ([`descent::superinstr`]).
//!
//! The profile corpus (here: two JSON documents of the shape the pack
//! sees in production) is mined for hot adjacent token-shift pairs into
//! a [`SuperSet`] — the data-driven synthesis of roadmap §1.3 #16, gated
//! to "only from PGO hot paths" (§10.2). The engine then runs the same
//! grammar with [`parse_events_with`]: every maximal token-shift run
//! whose adjacent pairs are hot executes as ONE fused dispatch instead
//! of per-token pattern recursion — the interpreter's analogue of the
//! generated parser's `parse_identifier_equals()` fusion.
//!
//! The conformance gate the example demonstrates: the fused run IS the
//! generic run, byte for byte (the §10.2 "conformance-gated" acceptance
//! criterion, pinned by `crates/descent/tests/superinstr.rs`).
//!
//! Run: `cargo run -p descent --example superinstr`

use descent::superinstr::mine;
use descent::{parse_events, parse_events_with};
use descent_lower::lower;
use gir_syntax::load_grammar;

/// The profile corpus: the §5.2 `profile/` idea — representative files
/// the pack is expected to parse in production.
const PROFILE: [&str; 2] = [r#"{"x": 1, "y": [2, 3]}"#, r#"{"z": {"w": true}}"#];

/// The document to parse (same shape, not part of the profile).
const DOCUMENT: &str = r#"{"name": "syntact", "tags": [1, 2, 3], "ok": true}"#;

fn main() {
    let grammar = load_grammar(descent_lower::JSON_GRAMMAR).expect("fixture loads");
    let lowered = lower(&grammar).expect("fixture lowers");
    let scanner = lexgen::compile(&grammar).expect("fixture lexes");

    // 1. Mine: profile feeds → hot token-shift pairs.
    let feeds: Vec<Vec<(u16, std::ops::Range<u32>)>> = PROFILE
        .iter()
        .map(|source| lowered.feed(&scanner, source))
        .collect();
    let refs: Vec<&[(u16, std::ops::Range<u32>)]> =
        feeds.iter().map(std::vec::Vec::as_slice).collect();
    let set = mine(&refs, 4, 64);

    // 2. The mining report: hottest first, count-descending (diff-stable).
    println!("mined {} hot token-shift pairs:", set.len());
    for entry in set.entries().iter().take(8) {
        println!(
            "  kind {} -> kind {}  x{}",
            entry.kinds[0], entry.kinds[1], entry.count
        );
    }

    // 3. Parse the document both ways and gate the identity.
    let feed = lowered.feed(&scanner, DOCUMENT);
    let generic = parse_events(&lowered.desc, &feed, DOCUMENT);
    let fused = parse_events_with(&lowered.desc, &feed, DOCUMENT, Some(&set));
    println!(
        "generic: {} events, {} tokens, {} errors",
        generic.events.len(),
        generic.tokens.len(),
        generic.errors
    );
    println!(
        "fused:   {} events, {} tokens, {} errors",
        fused.events.len(),
        fused.tokens.len(),
        fused.errors
    );
    assert_eq!(generic, fused, "the fused run IS the generic run");
    println!("conformance gate: fused == generic (byte for byte)");
}
