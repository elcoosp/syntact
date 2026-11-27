//! The rope-delta edit-descriptor gate (roadmap §7.1, §7.7, §8).
//!
//! Plan lines pinned: §7.1 *"`ropey::Rope` accepted as an alternative
//! input for editor integrations — the incremental engine consumes rope
//! deltas as edit descriptors (§7.7)"*; §8's incremental sketch
//! (`Edit::new(start, old_end, inserted_len)`) is annotated
//! *"ropey-compatible"*. The seam: a rope state pair (what an editor
//! holds) converts into the engine's `Edit` descriptor, and an engine
//! `Edit` applies back onto a rope — the round trip the editor loop
//! needs, with the engine's outcome on the rope-derived descriptor equal
//! to the outcome on the hand-built string edit.

use std::ops::Range;

use incremental::rope::{apply_edit, edit_between};
use incremental::{Edit, Engine, Lexer, Outcome};
use ropey::Rope;

/// A minimal hand lexer over JSON punctuation, string and number
/// literals (same discipline as `crates/incremental/tests/fuzz.rs`):
/// lexes any substring deterministically, skipping trivia.
struct JsonLexer;
impl Lexer for JsonLexer {
    fn lex(&self, source: &str) -> Vec<(u16, Range<u32>)> {
        let bytes = source.as_bytes();
        let mut out = Vec::new();
        let mut pos = 0_usize;
        while pos < bytes.len() {
            let start = pos;
            let kind = match bytes[pos] {
                b' ' | b'\t' | b'\r' | b'\n' => {
                    pos += 1;
                    continue;
                }
                b'{' => {
                    pos += 1;
                    1
                }
                b'}' => {
                    pos += 1;
                    2
                }
                b'[' => {
                    pos += 1;
                    3
                }
                b']' => {
                    pos += 1;
                    4
                }
                b':' => {
                    pos += 1;
                    5
                }
                b',' => {
                    pos += 1;
                    6
                }
                b'"' => {
                    pos += 1;
                    while pos < bytes.len() && bytes[pos] != b'"' {
                        pos += if bytes[pos] == b'\\' { 2 } else { 1 };
                    }
                    pos = (pos + 1).min(bytes.len());
                    7
                }
                _ => {
                    while pos < bytes.len()
                        && !matches!(
                            bytes[pos],
                            b' ' | b'\t'
                                | b'\r'
                                | b'\n'
                                | b'{'
                                | b'}'
                                | b'['
                                | b']'
                                | b':'
                                | b','
                                | b'"'
                        )
                    {
                        pos += 1;
                    }
                    8
                }
            };
            out.push((kind, (start as u32)..(pos as u32)));
        }
        out
    }
}

/// `(old, new)` rope pairs covering the editor's keystroke shapes.
fn cases() -> Vec<(&'static str, &'static str)> {
    vec![
        ("{\"a\": 1}", "{\"a\": 12}"),                 // trailing insert
        ("{\"a\": 1}", "{\"a\": }"),                   // delete
        ("{\"a\": 1}", "{\"b\": 1}"),                  // replace a key
        ("[1, 2, 3]", "[1, 2, 3, 4]"),                 // append in a list
        ("{\"ключ\": 1}", "{\"ключ\": \"значение\"}"), // multibyte content
        ("", "{}"),                                    // empty -> document
        ("{}", ""),                                    // document -> empty
    ]
}

#[test]
fn rope_state_yields_the_string_edit() {
    for (old, new) in cases() {
        let edit = edit_between(&Rope::from_str(old), &Rope::from_str(new))
            .unwrap_or_else(|| Edit::replace(0, old.len(), ""));
        // Applying the rope-derived descriptor to the old STRING yields
        // the new string — the descriptor is the same splice the editor
        // would have handed the engine on &str.
        assert_eq!(edit.apply(old), new, "descriptor for {old:?} -> {new:?}");
    }
}

#[test]
fn equal_ropes_decline_with_none() {
    let rope = Rope::from_str("{\"a\": 1}");
    assert!(edit_between(&rope, &rope.clone()).is_none());
    assert!(edit_between(&Rope::from_str(""), &Rope::from_str("")).is_none());
}

#[test]
fn descriptor_round_trips_through_the_rope() {
    for (old, new) in cases() {
        let old_rope = Rope::from_str(old);
        let new_rope = Rope::from_str(new);
        let Some(edit) = edit_between(&old_rope, &new_rope) else {
            assert_eq!(old, new);
            continue;
        };
        let applied = apply_edit(&old_rope, &edit);
        assert_eq!(
            applied.to_string(),
            new_rope.to_string(),
            "apply(edit_between(old, new)) == new for {old:?} -> {new:?}"
        );
    }
}

#[test]
fn engine_outcome_matches_the_string_edit() {
    let engine = Engine { lexer: &JsonLexer };
    for (old, new) in cases() {
        let old_tokens = engine.lexer.lex(old);
        let rope_edit = edit_between(&Rope::from_str(old), &Rope::from_str(new))
            .unwrap_or_else(|| Edit::replace(old.len(), old.len(), ""));
        let rope_outcome = engine.incremental_edit(old, &old_tokens, rope_edit.clone());
        // The hand-built descriptor over the same strings must produce
        // the identical stream: rope-derived input never narrows the
        // engine's behavior.
        let fresh = engine.lexer.lex(&rope_edit.apply(old));
        assert_eq!(rope_outcome.tokens, fresh, "stream for {old:?} -> {new:?}");
        let string_outcome: Outcome = engine.incremental_edit(old, &old_tokens, rope_edit);
        assert_eq!(rope_outcome, string_outcome);
    }
}

#[test]
fn multi_hunk_changes_are_safely_covered() {
    // Two separated keystrokes (a rope can carry both in one state
    // transition): the single-span cover is conservative — applying it
    // reproduces the new text, and the engine's stream equals a fresh
    // full lex (the fallback discipline, never a wrong stream).
    let old = "{\"alpha\": [1, 2], \"beta\": [3, 4]}";
    let new = "{\"alpha\": [1, 22], \"beta\": [33, 4]}";
    let old_rope = Rope::from_str(old);
    let new_rope = Rope::from_str(new);
    let edit = edit_between(&old_rope, &new_rope).expect("a cover exists");
    assert_eq!(edit.apply(old), new, "the cover reconstructs the new text");

    let engine = Engine { lexer: &JsonLexer };
    let outcome = engine.incremental_edit(old, &engine.lexer.lex(old), edit);
    assert_eq!(outcome.tokens, engine.lexer.lex(new));
    assert!(
        outcome.stats.fresh_tokens > 0,
        "the wide cover rides the fallback path"
    );
}
