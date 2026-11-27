//! The balanced-block fast path (roadmap §7.7 fast path 2): "small
//! balanced-brace block → re-parse block with state stamp; both fall
//! through to the general engine on mismatch". At the token-stream
//! engine's level the block re-parse is a block re-LEX + splice: the
//! smallest enclosing bracket-balanced region of the damage relexes
//! alone, with the same safety-predicate discipline as the margin-window
//! path (exact tiling, error-free fragment, boundary kind AND text
//! agreement), and every rejection falls through to the full relex.
//!
//! The acceptance criterion pinned here: a multi-token edit contained in
//! a balanced block relexes ONLY that block (a fast-path hit where the
//! single-token path must reject `MultiTokenDamage`), and the spliced
//! stream still equals a fresh lex of the new source — the issue 030
//! equivalence gate (`reparse(edit(x)) == fresh_parse(x)` at the token
//! level) holds on every path.

use std::ops::Range;

use incremental::{Edit, Engine, Lexer, Rejection};
use packs_json::kinds as jk;
use packs_json::reference::token_kinds as jt;

/// A real hand lexer over the JSON pack's kind constants (the
/// `fuzz.rs` fixture — lexes any substring deterministically).
struct JsonPackLexer;
impl Lexer for JsonPackLexer {
    fn lex(&self, source: &str) -> Vec<(u16, Range<u32>)> {
        let bytes = source.as_bytes();
        let mut out: Vec<(u16, Range<u32>)> = Vec::new();
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
                    jt::LBRACE
                }
                b'}' => {
                    pos += 1;
                    jt::RBRACE
                }
                b'[' => {
                    pos += 1;
                    jt::LBRACK
                }
                b']' => {
                    pos += 1;
                    jt::RBRACK
                }
                b':' => {
                    pos += 1;
                    jt::COLON
                }
                b',' => {
                    pos += 1;
                    jt::COMMA
                }
                b'"' => {
                    pos += 1;
                    while pos < bytes.len() {
                        if bytes[pos] == b'\\' {
                            pos += 2;
                            continue;
                        }
                        if bytes[pos] == b'"' {
                            pos += 1;
                            break;
                        }
                        pos += 1;
                    }
                    jk::STRING
                }
                b'0'..=b'9' | b'-' => {
                    while pos < bytes.len() && (bytes[pos].is_ascii_digit() || bytes[pos] == b'.') {
                        pos += 1;
                    }
                    jk::NUMBER
                }
                _ => {
                    let rest = &source[pos..];
                    let keyword = if rest.starts_with("true") {
                        Some((jk::TRUE, 4))
                    } else if rest.starts_with("false") {
                        Some((jk::FALSE, 5))
                    } else if rest.starts_with("null") {
                        Some((jk::NULL, 4))
                    } else {
                        None
                    };
                    if let Some((kind, len)) = keyword {
                        pos += len;
                        kind
                    } else {
                        pos += 1;
                        65534 // ERROR_KIND: garbage rides its own token
                    }
                }
            };
            let start32 = u32::try_from(start).unwrap_or(u32::MAX);
            let pos32 = u32::try_from(pos).unwrap_or(u32::MAX);
            out.push((kind, start32..pos32));
        }
        out
    }
}

/// The JSON block pairs: objects and arrays.
const BRACKETS: &[(u16, u16)] = &[(jt::LBRACE, jt::RBRACE), (jt::LBRACK, jt::RBRACK)];

#[test]
fn multi_token_block_edits_hit_the_block_path() {
    let engine = Engine {
        lexer: &JsonPackLexer,
    };
    let source = r#"{"a": [1, 2, 3], "b": {"c": 4}}"#;
    let tokens = JsonPackLexer.lex(source);
    // Multi-token damage inside the array block: five damaged tokens.
    let start = source.find("1, 2, 3").expect("the list");
    let edit = Edit::replace(start, start + 7, "10, 20, 30, 40");

    let new_source = edit.apply(source);
    // Without the block path, multi-token damage always falls back.
    let plain = engine.incremental_edit(source, &tokens, edit.clone());
    assert_eq!(
        plain.rejection,
        Some(Rejection::MultiTokenDamage),
        "the single-token path cannot take this edit"
    );

    // With the block pairs, the smallest enclosing block relexes alone.
    let outcome = engine.incremental_edit_with_brackets(source, &tokens, edit.clone(), BRACKETS);
    assert_eq!(
        outcome.rejection, None,
        "the block path takes the edit: {:?}",
        outcome.stats
    );
    assert_eq!(
        outcome.stats.fast_path_hits, 1,
        "the block splice is a fast-path hit"
    );
    assert_eq!(
        outcome.tokens,
        JsonPackLexer.lex(&new_source),
        "the spliced stream equals a fresh lex"
    );
    // The tokens outside the damaged window survived the splice.
    assert_eq!(outcome.stats.reused_tokens, tokens.len() - 5);
}

#[test]
fn nested_edits_relex_their_own_block() {
    // The edit sits inside the INNER object; the block path must still
    // be exact (and still a hit), even though two enclosing blocks
    // could claim the damage.
    let engine = Engine {
        lexer: &JsonPackLexer,
    };
    let source = r#"{"outer": {"inner": [1, 2]}}"#;
    let tokens = JsonPackLexer.lex(source);
    let start = source.find("1, 2").expect("the inner list");
    let edit = Edit::replace(start, start + 4, "true, null");
    let new_source = edit.apply(source);
    let outcome = engine.incremental_edit_with_brackets(source, &tokens, edit, BRACKETS);
    assert_eq!(outcome.rejection, None, "{:?}", outcome.stats);
    assert_eq!(outcome.tokens, JsonPackLexer.lex(&new_source));
}

#[test]
fn single_token_edits_still_take_the_margin_window() {
    // The bracket-aware entry keeps the margin-window behavior for
    // one-token damage — the block scan never runs.
    let engine = Engine {
        lexer: &JsonPackLexer,
    };
    let source = r#"{"a": 1, "b": 2}"#;
    let tokens = JsonPackLexer.lex(source);
    let at = source.find('2').expect("the 2");
    let edit = Edit::replace(at, at + 1, "42");
    let new_source = edit.apply(source);
    let outcome = engine.incremental_edit_with_brackets(source, &tokens, edit, BRACKETS);
    assert_eq!(outcome.rejection, None);
    assert_eq!(outcome.stats.fast_path_hits, 1);
    assert_eq!(outcome.tokens, JsonPackLexer.lex(&new_source));
}

#[test]
fn boundary_crossing_edits_fall_through_exactly() {
    // The edit spans from inside one block across its closer into the
    // next sibling — no enclosing block contains the damage; the
    // predicates reject and the fallback relexes everything (still
    // exact).
    let engine = Engine {
        lexer: &JsonPackLexer,
    };
    let source = r#"{"a": [1, 2], "b": 3}"#;
    let tokens = JsonPackLexer.lex(source);
    let start = source.find("2]").expect("the tail");
    let end = source.find(", \"b\"").expect("the sibling boundary");
    let edit = Edit::replace(start, end, "9");
    let new_source = edit.apply(source);
    let outcome = engine.incremental_edit_with_brackets(source, &tokens, edit, BRACKETS);
    assert!(
        outcome.rejection.is_some(),
        "the block path must fall through: {:?}",
        outcome.stats
    );
    assert_eq!(outcome.tokens, JsonPackLexer.lex(&new_source));
}

#[test]
fn closer_deleting_edits_stay_exact_on_every_path() {
    // Deleting a trailing closer is one damaged token — the margin
    // window takes it; deleting a mid-stream closer relexes the wide
    // window around it. Whichever path wins, the spliced stream is
    // exact (the accelerator never changes the answer).
    let engine = Engine {
        lexer: &JsonPackLexer,
    };
    for source in [r#"{"a": [1, 2]}"#, r#"{"a": [1, 2], "b": 3}"#] {
        let tokens = JsonPackLexer.lex(source);
        let at = source.rfind(']').expect("the closer");
        let edit = Edit::delete(at, at + 1);
        let new_source = edit.apply(source);
        let outcome = engine.incremental_edit_with_brackets(source, &tokens, edit, BRACKETS);
        assert_eq!(
            outcome.tokens,
            JsonPackLexer.lex(&new_source),
            "the spliced stream equals a fresh lex: {source}"
        );
    }
}

#[test]
fn quote_merging_edits_reject_the_context() {
    // Inserting a quote inside a string merges it with what follows —
    // the relexed fragment can no longer reproduce the context (the
    // same context-hash discipline as the margin window; the `fuzz.rs`
    // precedent, now through the bracket-aware entry).
    let engine = Engine {
        lexer: &JsonPackLexer,
    };
    let source = r#"{"key with spaces": [1]}"#;
    let tokens = JsonPackLexer.lex(source);
    let at = source.find("spaces").expect("inside the string");
    let edit = Edit::insert(at, "\"");
    let new_source = edit.apply(source);
    let outcome = engine.incremental_edit_with_brackets(source, &tokens, edit, BRACKETS);
    assert_eq!(
        outcome.rejection,
        Some(Rejection::ContextMismatch),
        "a quote-merging edit must reject: {:?}",
        outcome.stats
    );
    assert_eq!(outcome.tokens, JsonPackLexer.lex(&new_source));
}

#[test]
fn seeded_block_fuzz_keeps_the_equivalence_gate() {
    // Deterministic xorshift64* (the fuzz.rs discipline): repeated
    // multi-token edits inside balanced regions — the spliced stream
    // equals a fresh lex on every case, hits or rejections.
    struct Rng(u64);
    impl Rng {
        fn new(seed: u64) -> Self {
            Self(seed | 1)
        }
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn below(&mut self, bound: usize) -> usize {
            usize::try_from(self.next() % u64::try_from(bound).unwrap_or(u64::MAX)).unwrap_or(0)
        }
    }

    let engine = Engine {
        lexer: &JsonPackLexer,
    };
    let base = r#"{"a": [1, 2, 3], "b": {"c": [true, null]}, "d": 4}"#;
    let mut source = base.to_owned();
    let mut rng = Rng::new(0x5EED_1234_ABCD);
    let fragments = [
        "9",
        "10, 20",
        "{\"x\": 1}",
        "[false]",
        "\"s\"",
        "true",
        "",
        "1, 2, 3, 4, 5, 6, 7",
    ];
    let mut hits = 0_usize;
    for _ in 0..200 {
        let tokens = JsonPackLexer.lex(&source);
        if tokens.is_empty() {
            break;
        }
        // A random token, a random span from it, a random fragment —
        // damage of every shape, most of it block-contained.
        let index = rng.below(tokens.len());
        let (_, range) = &tokens[index];
        let start = range.start as usize;
        let span = 1 + rng.below(3);
        let end = (start + span).min(source.len());
        let fragment = fragments[rng.below(fragments.len())];
        let edit = Edit::replace(start, end, fragment);
        let new_source = edit.apply(source.as_str());
        let outcome =
            engine.incremental_edit_with_brackets(source.as_str(), &tokens, edit.clone(), BRACKETS);
        assert_eq!(
            outcome.tokens,
            JsonPackLexer.lex(&new_source),
            "equivalence holds on every path"
        );
        if outcome.rejection.is_none() {
            hits += 1;
        }
        source = new_source;
    }
    assert!(hits > 0, "the fuzz corpus must exercise the block path");
}
