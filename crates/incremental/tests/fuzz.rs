//! incremental integration tests (issue 030 TDD): fast-path trigger
//! stats, reuse-predicate rejections, changed ranges vs the brute-force
//! diff, and seeded incremental-equivalence fuzzing on the json+toml
//! packs — `reparse(edit(x)) == fresh_parse(x)` at the token-stream
//! level for every mutation.

use std::ops::Range;

use incremental::{Edit, Engine, Lexer, Rejection, reused_subtree_bytes};
use packs_json::kinds as jk;
use packs_json::reference::token_kinds as jt;

/// A real hand lexer over the JSON pack's kind constants. Unlike a
/// whole-document parse it lexes any substring deterministically —
/// exactly what the margin-window fast path needs.
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

/// The TOML pack's token stream via a whole-document parse — the
/// fallback-driven fuzz oracle (fast-path windows may reject; the
/// equivalence gates still hold).
struct TomlParseLexer;
impl Lexer for TomlParseLexer {
    fn lex(&self, source: &str) -> Vec<(u16, Range<u32>)> {
        packs_toml::reference::parse_events(source).1
    }
}

/// Deterministic xorshift64* — no external RNG dependency.
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
        if bound == 0 {
            0
        } else {
            let bound64 = u64::try_from(bound).unwrap_or(u64::MAX);
            usize::try_from(self.next() % bound64).unwrap_or(0)
        }
    }
}

#[test]
fn fast_path_hits_on_single_token_edits() {
    let engine = Engine {
        lexer: &JsonPackLexer,
    };
    let source = r#"{"a": 1, "b": 2}"#;
    let tokens = JsonPackLexer.lex(source);
    // Replace the `2` literal — one damaged token, flanks intact.
    let at = source.find('2').expect("the 2");
    let outcome = engine.incremental_edit(source, &tokens, Edit::replace(at, at + 1, "42"));
    assert_eq!(outcome.rejection, None, "stats: {:?}", outcome.stats);
    assert_eq!(outcome.stats.fast_path_hits, 1);
    assert_eq!(
        outcome.tokens,
        JsonPackLexer.lex(r#"{"a": 1, "b": 42}"#),
        "spliced stream equals a fresh lex"
    );
}

#[test]
fn fast_path_rejects_context_breaking_edits() {
    // Inserting a quote right after a string literal merges the two:
    // the wide-window relex's LAST token (a runaway string) can no
    // longer match the old outer flank — the predicate rejects and the
    // fallback relexes everything.
    let engine = Engine {
        lexer: &JsonPackLexer,
    };
    let source = r#"{"a": 1}"#;
    let tokens = JsonPackLexer.lex(source);
    let outcome = engine.incremental_edit(source, &tokens, Edit::insert(4, "\""));
    assert_eq!(
        outcome.rejection,
        Some(Rejection::ContextMismatch),
        "string-merging edits must reject: {:?}",
        outcome.stats
    );
    assert_eq!(outcome.stats.fast_path_rejections, 1);
    // Correctness holds anyway: the fallback stream is exact.
    assert_eq!(outcome.tokens, JsonPackLexer.lex(r#"{"a"": 1}"#));
}

#[test]
fn string_internal_edits_hit_the_fast_path() {
    // The margin window CONTAINS the whole string literal, so the relex
    // reproduces it exactly: a legitimate fast-path hit.
    let engine = Engine {
        lexer: &JsonPackLexer,
    };
    let source = r#"{"key with spaces": 1}"#;
    let tokens = JsonPackLexer.lex(source);
    let at = source.find("spaces").expect("inside the string");
    let outcome = engine.incremental_edit(source, &tokens, Edit::replace(at, at + 6, "!!!"));
    assert_eq!(outcome.rejection, None);
    assert_eq!(outcome.stats.fast_path_hits, 1);
    assert_eq!(outcome.tokens, JsonPackLexer.lex(r#"{"key with !!!": 1}"#));
}

#[test]
fn multi_token_damage_falls_back_and_counts_reuse() {
    let engine = Engine {
        lexer: &JsonPackLexer,
    };
    let source = r"[1, 2, 3, 4, 5]";
    let tokens = JsonPackLexer.lex(source);
    // Replace a whole span covering several tokens.
    let start = source.find("2,").expect("the 2,");
    let end = source.find('4').expect("the 4");
    let outcome = engine.incremental_edit(source, &tokens, Edit::replace(start, end + 1, "9"));
    assert_eq!(outcome.rejection, Some(Rejection::MultiTokenDamage));
    assert!(outcome.stats.reused_tokens > 0, "the tail survives");
    assert_eq!(outcome.tokens, JsonPackLexer.lex("[1, 9, 5]"));
}

#[test]
fn changed_ranges_match_a_fresh_lex_diff() {
    let engine = Engine {
        lexer: &JsonPackLexer,
    };
    let source = r#"{"a": [1, 2, 3], "b": null}"#;
    let tokens = JsonPackLexer.lex(source);
    let at = source.find("null").expect("null");
    let outcome = engine.incremental_edit(source, &tokens, Edit::replace(at, at + 4, "false"));
    let new_source = r#"{"a": [1, 2, 3], "b": false}"#;
    // The engine's diff equals a brute-force diff of two full lexes.
    let brute = incremental::diff::changed_ranges(
        &tokens,
        source,
        &JsonPackLexer.lex(new_source),
        new_source,
    );
    assert_eq!(outcome.changed, brute);
    // Reused subtrees: everything outside the changed range.
    let reused = reused_subtree_bytes(&tokens, &outcome.changed);
    let total: usize = tokens.iter().map(|(_, r)| (r.end - r.start) as usize).sum();
    assert!(reused < total, "some bytes changed");
    assert!(reused > 0, "most bytes survived");
}

/// Seeded fuzz on the JSON pack: mutations of valid documents keep the
/// incremental stream equal to a fresh lex, every time.
#[test]
fn fuzz_json_incremental_equivalence() {
    let engine = Engine {
        lexer: &JsonPackLexer,
    };
    let corpus = [
        r#"{"a": 1}"#,
        r"[1, 2, 3]",
        r#"{"deep": {"deeper": [true, null]}, "b": "x"}"#,
        r#"[{"k": "v"}, [1, [2]], 42]"#,
        r#""top level string""#,
    ];
    let mut rng = Rng::new(0x0DEC_0DE5);
    for round in 0..200 {
        let source = corpus[round % corpus.len()];
        let tokens = JsonPackLexer.lex(source);
        let edit = random_edit(&mut rng, source);
        let new_source = edit.apply(source);
        let outcome = engine.incremental_edit(source, &tokens, edit.clone());
        if std::env::var("FUZZ_TRACE").is_ok() {
            println!("round {round} edit={edit:?} -> {new_source:?}");
        }
        // Incremental equivalence: the spliced stream IS the fresh lex.
        assert_eq!(
            outcome.tokens,
            JsonPackLexer.lex(&new_source),
            "round {round}: stream mismatch on {new_source:?}"
        );
        // The changed ranges match the brute-force diff.
        let brute = incremental::diff::changed_ranges(
            &tokens,
            source,
            &JsonPackLexer.lex(&new_source),
            &new_source,
        );
        assert_eq!(outcome.changed, brute, "round {round}: diff mismatch");
    }
}

/// Seeded fuzz on the TOML pack (same gates, parse-table lexer).
#[test]
fn fuzz_toml_incremental_equivalence() {
    let engine = Engine {
        lexer: &TomlParseLexer,
    };
    let corpus = [
        "a = 1\nb = \"x\"\n",
        "[table]\nx = [1, 2, 3]\n",
        "a.b.c = true\nflag = false\n",
        "list = [\n  1, # comment\n  2,\n]\n",
    ];
    let mut rng = Rng::new(0xFEED_F00D);
    for round in 0..100 {
        let source = corpus[round % corpus.len()];
        let tokens = TomlParseLexer.lex(source);
        let edit = random_edit(&mut rng, source);
        let new_source = edit.apply(source);
        println!("toml round {round} edit={edit:?} src={new_source:?}");
        let outcome = engine.incremental_edit(source, &tokens, edit.clone());
        assert_eq!(
            outcome.tokens,
            TomlParseLexer.lex(&new_source),
            "round {round}: stream mismatch on {new_source:?}"
        );
        let brute = incremental::diff::changed_ranges(
            &tokens,
            source,
            &TomlParseLexer.lex(&new_source),
            &new_source,
        );
        assert_eq!(outcome.changed, brute, "round {round}: diff mismatch");
    }
}

/// A random single-point mutation: insert / delete / replace a short
/// byte run at a random position.
fn random_edit(rng: &mut Rng, source: &str) -> Edit {
    let len = source.len();
    let start = rng.below(len + 1);
    match rng.next() % 3 {
        0 => {
            // insert one of a few structural characters
            let chars = ["x", "1", ",", "\"", "}", " ", ":"];
            let text = chars[rng.below(chars.len())];
            Edit::insert(start, text)
        }
        1 => {
            let end = (start + 1 + rng.below(3)).min(len);
            Edit::delete(start, end)
        }
        _ => {
            let end = (start + 1 + rng.below(3)).min(len);
            Edit::replace(start, end, "z9")
        }
    }
}
