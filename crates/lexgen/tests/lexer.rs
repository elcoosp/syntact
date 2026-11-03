//! lexgen integration tests (issue 014 TDD): per-pattern shapes, keyword
//! strategy, longest-match, zero-length alignment, error progress and
//! number regexes.

use gir_ir::GrammarBuilder;
use lexgen::{Lexeme, compile};

/// Build a grammar from (name, pattern, trivia) triples.
fn grammar_of(tokens: &[(&str, &str, bool)]) -> gir_ir::Grammar {
    let mut b = GrammarBuilder::new("lexer-fixture");
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
    // The grammar IR requires at least one rule; the lexer only reads the
    // token table, so a trivial rule over the first token suffices.
    let first_name = tokens[0].0;
    let first = b.token_id(first_name).expect("token");
    let lex = b.declare_rule("lex").expect("r");
    b.set_body(lex, GrammarBuilder::token_ref(first));
    b.finish().expect("valid")
}

/// Token lexeme texts in order (errors render as `<???>`).
fn rendered(scanner: &lexgen::Scanner, source: &str) -> Vec<String> {
    scanner
        .scan(source)
        .iter()
        .map(|lexeme| match lexeme {
            Lexeme::Token { range, .. } => {
                source[range.start as usize..range.end as usize].to_owned()
            }
            Lexeme::Error { range } => {
                format!("<???>{}", &source[range.start as usize..range.end as usize])
            }
        })
        .collect()
}

#[test]
fn lit_and_regex_tokens_match() {
    let grammar = grammar_of(&[
        ("lbrace", "{", false),
        ("ident", "(re)[a-z]+", false),
        ("ws", "(re)\\s+", true),
    ]);
    let scanner = compile(&grammar).expect("compiles");
    let lexemes = scanner.scan("{ ab c");
    assert_eq!(rendered(&scanner, "{ ab c"), ["{", " ", "ab", " ", "c"]);
    assert!(lexemes.iter().all(|l| matches!(l, Lexeme::Token { .. })));
    // Trivia flag rides along.
    let trivia_flags: Vec<bool> = lexemes
        .iter()
        .map(|l| matches!(l, Lexeme::Token { trivia, .. } if *trivia))
        .collect();
    assert_eq!(trivia_flags, [false, true, false, true, false]);
}

#[test]
fn keyword_strategy_declaration_order_wins_ties() {
    // Keyword declared before the identifier regex: equal length →
    // keyword wins.
    let grammar = grammar_of(&[("kw_if", "if", false), ("ident", "(re)[a-z]+", false)]);
    let scanner = compile(&grammar).expect("compiles");
    assert_eq!(rendered(&scanner, "if"), ["if"]);
    // But longest match still beats priority: `iffy` is one identifier.
    assert_eq!(rendered(&scanner, "iffy"), ["iffy"]);
    // And `if x` splits at the whitespace boundary (no match → error).
    assert_eq!(rendered(&scanner, "ifx"), ["ifx"]);
}

#[test]
fn number_regexes() {
    let grammar = grammar_of(&[
        ("float", "(re)[0-9]+\\.[0-9]+", false),
        ("int", "(re)[0-9]+", false),
        ("hex", "(re)0x[0-9a-fA-F]+", false),
    ]);
    let scanner = compile(&grammar).expect("compiles");
    // Longest match picks the float over the int prefix.
    assert_eq!(rendered(&scanner, "3.14"), ["3.14"]);
    assert_eq!(rendered(&scanner, "42"), ["42"]);
    assert_eq!(rendered(&scanner, "0xDEADbeef"), ["0xDEADbeef"]);
    // `x` matches nothing → error lexeme.
    assert_eq!(rendered(&scanner, "1.5x2"), ["1.5", "<???>x", "2"]);
}

#[test]
fn bounded_repetition() {
    let grammar = grammar_of(&[("bin", "(re)(01|10){2,3}", false)]);
    let scanner = compile(&grammar).expect("compiles");
    assert_eq!(rendered(&scanner, "0110"), ["0110"]);
    assert_eq!(rendered(&scanner, "011001"), ["011001"]);
    // Only one alternation pair: no match (needs ≥2) → error lexemes.
    assert_eq!(rendered(&scanner, "01"), ["<???>0", "<???>1"]);
}

#[test]
fn zero_length_matches_never_stall() {
    // `a*` matches ε: the scanner must not loop; positions advance via
    // longest non-empty match or error lexemes.
    let grammar = grammar_of(&[("loop", "(re)a*", false), ("ws", "(re)\\s+", true)]);
    let scanner = compile(&grammar).expect("compiles");
    let lexemes = scanner.scan("aa b");
    // `b` matches nothing in this grammar → error lexeme; still exactly
    // three lexemes (no ε stall from `a*`).
    assert_eq!(rendered(&scanner, "aa b"), ["aa", " ", "<???>b"]);
    assert_eq!(lexemes.len(), 3);
}

#[test]
fn unmatched_chars_make_progress_and_stay_lossless() {
    let grammar = grammar_of(&[("ab", "(re)[ab]+", false)]);
    let scanner = compile(&grammar).expect("compiles");
    let source = "ab€cd!";
    let lexemes = scanner.scan(source);
    // Coverage: lexeme ranges + gaps partition the source.
    let mut covered = 0u32;
    for lexeme in &lexemes {
        let range = match lexeme {
            Lexeme::Token { range, .. } | Lexeme::Error { range } => range,
        };
        assert_eq!(range.start, covered, "contiguous coverage");
        covered = range.end;
    }
    assert_eq!(covered as usize, source.len());
    // € and ! are error lexemes; c/d are outside `[ab]+` too.
    assert_eq!(
        rendered(&scanner, source),
        ["ab", "<???>€", "<???>c", "<???>d", "<???>!"]
    );
}

#[test]
fn multibyte_chars_scan_by_scalar() {
    let grammar = grammar_of(&[("word", "(re)[éàü]+", false)]);
    let scanner = compile(&grammar).expect("compiles");
    assert_eq!(rendered(&scanner, "éàü"), ["éàü"]);
    assert_eq!(rendered(&scanner, "éa"), ["é", "<???>a"]);
}

#[test]
fn empty_source_scans_to_nothing() {
    let grammar = grammar_of(&[("a", "a", false)]);
    let scanner = compile(&grammar).expect("compiles");
    assert_eq!(scanner.scan(""), [] as [Lexeme; 0]);
}

#[test]
fn scanner_knows_its_token_count() {
    let grammar = grammar_of(&[("a", "a", false), ("bb", "bb", true)]);
    let scanner = compile(&grammar).expect("compiles");
    assert_eq!(scanner.token_count(), 2);
}

#[test]
fn merged_automata_do_not_interfere() {
    let grammar = grammar_of(&[
        ("lbrace", "{", false),
        ("rbrace", "}", false),
        ("ident", "(re)[a-zA-Z_][a-zA-Z_0-9]*", false),
        ("number", "(re)[0-9]+", false),
        ("ws", "(re) +", true),
    ]);
    let scanner = compile(&grammar).expect("compiles");
    assert_eq!(
        rendered(&scanner, "{ x9 42 }"),
        ["{", " ", "x9", " ", "42", " ", "}"]
    );
}

#[test]
fn nested_quantifiers_compile_and_match() {
    let grammar = grammar_of(&[("tuple", "(re)(ab)+", false), ("opt", "(re)x?y", false)]);
    let scanner = compile(&grammar).expect("compiles");
    assert_eq!(rendered(&scanner, "ababab"), ["ababab"]);
    assert_eq!(rendered(&scanner, "y"), ["y"]);
    assert_eq!(rendered(&scanner, "xy"), ["xy"]);
    assert_eq!(rendered(&scanner, "abxy"), ["ab", "xy"]);
}
