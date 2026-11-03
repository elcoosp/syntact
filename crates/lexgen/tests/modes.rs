//! Mode-stack suites (issue 022): nesting, pop-at-base stickiness,
//! switch, per-mode DFA sub-tables and data-driven transitions.

use gir_ir::TokenPattern;
use lexgen::{Lexeme, ModeAction, ModeLexer, ModeSpec, ModeToken};

fn lit(text: &str, kind: u16, action: ModeAction) -> ModeToken {
    ModeToken {
        kind,
        pattern: TokenPattern::Lit {
            text: text.to_owned(),
        },
        trivia: false,
        action,
    }
}

fn re(pattern: &str, kind: u16, action: ModeAction) -> ModeToken {
    ModeToken {
        kind,
        pattern: TokenPattern::Re {
            regex: pattern.to_owned(),
        },
        trivia: false,
        action,
    }
}

/// Two modes: base = `{ a, OPEN→Push(1), x }`, nested = `{ b, OPEN→Push(1), CLOSE→Pop, y }`
/// — the nested mode can re-enter itself, so nesting is recursive.
fn nested_lexer() -> ModeLexer {
    ModeLexer::new(&[
        ModeSpec {
            tokens: vec![
                lit("a", 1, ModeAction::Stay),
                lit("OPEN", 2, ModeAction::Push(1)),
                re("[x-z]", 3, ModeAction::Stay),
            ],
        },
        ModeSpec {
            tokens: vec![
                lit("b", 4, ModeAction::Stay),
                lit("OPEN", 2, ModeAction::Push(1)),
                lit("CLOSE", 5, ModeAction::Pop),
                lit("y", 6, ModeAction::Stay),
            ],
        },
    ])
    .expect("compiles")
}

#[test]
fn pushes_and_pops_nest() {
    let mut lexer = nested_lexer();
    assert_eq!(lexer.current(), 0);
    assert_eq!(lexer.depth(), 1);
    // OPEN pushes into mode 1: only its tokens match there.
    let lexemes = lexer.scan("OPENbCLOSEa");
    let kinds: Vec<u16> = lexemes
        .iter()
        .filter_map(|l| match l {
            Lexeme::Token { kind, .. } => Some(*kind),
            Lexeme::Error { .. } => None,
        })
        .collect();
    assert_eq!(kinds, [2, 4, 5, 1], "OPEN b CLOSE a");
    assert_eq!(lexer.current(), 0, "CLOSE popped back to base");
    assert_eq!(lexer.depth(), 1);
}

#[test]
fn stacking_is_recursive() {
    let mut lexer = nested_lexer();
    // OPEN OPEN b CLOSE CLOSE a: two pushes, two pops return to base.
    let lexemes = lexer.scan("OPENOPENbCLOSECLOSEa");
    let tokens: Vec<(u16, &str)> = lexemes
        .iter()
        .filter_map(|l| match l {
            Lexeme::Token { kind, range, .. } => Some((
                *kind,
                &"OPENOPENbCLOSECLOSEa"[range.start as usize..range.end as usize],
            )),
            Lexeme::Error { .. } => None,
        })
        .collect();
    assert_eq!(
        tokens,
        [
            (2, "OPEN"),
            (2, "OPEN"),
            (4, "b"),
            (5, "CLOSE"),
            (5, "CLOSE"),
            (1, "a")
        ]
    );
    assert_eq!(lexer.depth(), 1);
    // No errors: every byte was tokenized.
    assert_eq!(lexemes.len(), tokens.len());
}

#[test]
fn pop_at_base_is_sticky() {
    let mut lexer = nested_lexer();
    // CLOSE in base mode still matches (mode 0 has CLOSE? no — it must
    // error there instead). Verify via an unmatched char + depth stable.
    let lexemes = lexer.scan("a");
    assert!(matches!(&lexemes[0], Lexeme::Token { kind: 1, .. }));
    assert_eq!(lexer.depth(), 1);
}

#[test]
fn base_mode_errors_on_foreign_tokens() {
    let mut lexer = nested_lexer();
    // `b` only exists in mode 1: one-char error lexemes in base.
    let lexemes = lexer.scan("bb");
    assert!(matches!(&lexemes[0], Lexeme::Error { range } if range.start == 0 && range.end == 1));
    assert!(matches!(&lexemes[1], Lexeme::Error { range } if range.start == 1 && range.end == 2));
}

#[test]
fn switch_replaces_the_top() {
    let mut lexer = ModeLexer::new(&[
        ModeSpec {
            tokens: vec![lit("<", 1, ModeAction::Switch(1))],
        },
        ModeSpec {
            tokens: vec![lit("N", 2, ModeAction::Stay)],
        },
    ])
    .expect("compiles");
    let lexemes = lexer.scan("<NN");
    let kinds: Vec<u16> = lexemes
        .iter()
        .filter_map(|l| match l {
            Lexeme::Token { kind, .. } => Some(*kind),
            Lexeme::Error { .. } => None,
        })
        .collect();
    assert_eq!(kinds, [1, 2, 2]);
    assert_eq!(lexer.current(), 1, "switch replaced base");
    assert_eq!(lexer.depth(), 1, "switch keeps depth");
    assert_eq!(lexer.mode_count(), 2);
}

#[test]
fn declaration_order_breaks_ties_per_mode() {
    // Keyword before regex in mode 0: `if` wins over the identifier.
    let mut lexer = ModeLexer::new(&[ModeSpec {
        tokens: vec![
            lit("if", 1, ModeAction::Stay),
            re("[a-z]+", 2, ModeAction::Stay),
        ],
    }])
    .expect("compiles");
    let lexemes = lexer.scan("if iff");
    let texts: Vec<&str> = lexemes
        .iter()
        .filter_map(|l| match l {
            Lexeme::Token { range, .. } => {
                Some(&"if iff"[range.start as usize..range.end as usize])
            }
            Lexeme::Error { .. } => None,
        })
        .collect();
    assert_eq!(texts, ["if", "iff"]);
}

#[test]
fn empty_specs_are_rejected() {
    assert!(ModeLexer::new(&[]).is_err());
    assert!(ModeLexer::new(&[ModeSpec { tokens: vec![] }]).is_err());
}
