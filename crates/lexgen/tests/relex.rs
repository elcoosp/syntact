//! Relex suites (issue 022): `relex_one` parity with full-scan prefixes,
//! boundary behavior, and independence from trailing edits.

use gir_ir::GrammarBuilder;
use lexgen::{Lexeme, compile};

/// Build a grammar from (name, pattern, trivia) triples — `(re)` prefixes
/// mark regex patterns (same fixture convention as `tests/lexer.rs`).
fn grammar_of(tokens: &[(&str, &str, bool)]) -> gir_ir::Grammar {
    let mut b = GrammarBuilder::new("relex-fixture");
    for (name, pattern, trivia) in tokens {
        let id = if let Some(regex) = pattern.strip_prefix("(re)") {
            b.token_re(name, regex)
        } else {
            b.token_lit(name, pattern)
        }
        .expect("token");
        if *trivia {
            b.set_trivia(id, true);
        }
    }
    let first = b.token_id(tokens[0].0).expect("token");
    let lex = b.declare_rule("lex").expect("r");
    b.set_body(lex, GrammarBuilder::token_ref(first));
    b.finish().expect("valid")
}

fn text_of<'a>(source: &'a str, lexeme: &Lexeme) -> &'a str {
    match lexeme {
        Lexeme::Token { range, .. } | Lexeme::Error { range } => {
            &source[range.start as usize..range.end as usize]
        }
    }
}

fn jsonish_grammar() -> gir_ir::Grammar {
    grammar_of(&[
        ("ws", "(re)[ ]+", true),
        ("kw_null", "null", false),
        ("kw_true", "true", false),
        ("ident", "(re)[a-z]+", false),
        ("int", "(re)[0-9]+", false),
        ("colon", ":", false),
        ("comma", ",", false),
    ])
}

#[test]
fn relex_matches_full_scan_prefixes() {
    let source = "null true alpha 12, beta:false ";
    let scanner = compile(&jsonish_grammar()).expect("compiles");
    let full = scanner.scan(source);
    assert!(full.len() > 5, "fixture exercises multiple shapes");
    for (idx, expected) in full.iter().enumerate() {
        let start = match expected {
            Lexeme::Token { range, .. } | Lexeme::Error { range } => range.start as usize,
        };
        let relexed = scanner.relex_one(source, start).expect("in bounds");
        assert_eq!(relexed, *expected, "relex at token {idx} start {start}");
        assert_eq!(text_of(source, &relexed), text_of(source, expected));
    }
}

#[test]
fn relex_is_immune_to_trailing_edits() {
    // The whole point: relexing offset 0 sees only text at/after the
    // offset — a different tail must not change the head token.
    let scanner = compile(&jsonish_grammar()).expect("compiles");
    let a = scanner.relex_one("true, 42", 0).expect("token");
    let b = scanner
        .relex_one("true!!!totally different tail", 0)
        .expect("token");
    assert_eq!(a, b);
    assert!(matches!(a, Lexeme::Token { .. }));
}

#[test]
fn relex_error_trivia_and_bounds() {
    let scanner = compile(&jsonish_grammar()).expect("compiles");
    // Unmatched char → one-char error lexeme (progress guarantee).
    let lexeme = scanner.relex_one("!alpha", 0).expect("in bounds");
    assert!(matches!(lexeme, Lexeme::Error { range } if range.start == 0 && range.end == 1));
    // Trivia classification rides along (whitespace is trivia).
    let ws = scanner.relex_one("   alpha", 0).expect("token");
    assert!(matches!(ws, Lexeme::Token { trivia: true, .. }));
    let kw = scanner.relex_one("null", 0).expect("token");
    assert!(matches!(kw, Lexeme::Token { trivia: false, .. }));
    // Out-of-bounds offsets are None, never a panic.
    assert_eq!(scanner.relex_one("abc", 3), None);
    assert_eq!(scanner.relex_one("abc", 99), None);
    assert_eq!(scanner.relex_one("", 0), None);
}

#[test]
fn relex_handles_multibyte_boundaries() {
    let scanner = compile(&jsonish_grammar()).expect("compiles");
    // `é` is two bytes: the error lexeme must cover the whole char.
    let source = "é alpha";
    let lexeme = scanner.relex_one(source, 0).expect("in bounds");
    assert!(matches!(lexeme, Lexeme::Error { range } if range.start == 0 && range.end == 2));
    // And relexing at the char boundary after it still works.
    let after = scanner.relex_one(source, 2).expect("token");
    assert!(matches!(after, Lexeme::Token { .. }), "alpha matches");
}
