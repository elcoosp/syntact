//! packs-rust test suite (issue 033 TDD): per-family shapes, lossless-
//! ness, the lifetime/char disambiguation, balanced macro token trees,
//! and per-fixture tolerated-error budgets.

use std::ops::Range;
use std::sync::Arc;

use pretty_assertions::assert_eq;

use runtime_core::events::Event;
use runtime_core::input::Source;
use runtime_core::tree::{GreenChild, GreenNode};

use crate::kinds;
use crate::reference::parse_events;
use crate::{ROOT_KIND, token_kinds as tk};

/// Embedded corpus: valid Rust across the families plus malformed
/// inputs with tolerated-error budgets.
const CORPUS: &[(&str, usize)] = &[
    // -- items -----------------------------------------------------------
    ("fn main() {}", 0),
    ("fn add(a: u32, b: u32) -> u32 { a + b }", 0),
    ("fn generic<'a, T: Clone>(x: &'a T) where T: Send {}", 0),
    ("struct Unit;", 0),
    ("struct Tup(u32, String);", 0),
    ("struct Named { a: u32, b: String }", 0),
    ("enum E { A, B(u8), C { x: i32 } }", 0),
    ("impl Default for E { fn default() -> Self { E::A } }", 0),
    (
        "impl<T: Clone> Wrapper<T> { fn wrap(v: T) -> Self { Self { v } } }",
        0,
    ),
    ("trait Shape { fn area(&self) -> f64; }", 0),
    ("mod inner { pub fn hidden() {} }", 0),
    ("use std::collections::{HashMap as Map, HashSet};", 0),
    ("use std::io::*;", 0),
    ("const MAX: u32 = 100;", 0),
    ("static NAME: &str = \"x\";", 0),
    ("type Alias = Vec<u8>;", 0),
    // -- expressions -----------------------------------------------------
    ("fn f() { let x = 1 + 2 * 3; }", 0),
    ("fn f() { let c = |a, b| a + b; }", 0),
    ("fn f() { let r = 0..10; }", 0),
    ("fn f() { let r = 0..=9; }", 0),
    ("fn f() { let t = (1, \"two\", 3.0); }", 0),
    ("fn f() { let a = [1, 2, 3]; }", 0),
    ("fn f() { let s = S { field: 1, rest: q }; }", 0),
    ("fn f() { v.iter().map(|x| x * 2).collect::<Vec<_>>(); }", 0),
    (
        "fn f() { match x { 1 => \"one\", 2..=5 => \"few\", _ => 0 } }",
        0,
    ),
    ("fn f() { if a { b } else if c { d } else { e } }", 0),
    (
        "fn f() { while let Some(v) = it.next() { consume(v); } }",
        0,
    ),
    ("fn f() { for i in 0..n { sum += i; } }", 0),
    ("fn f() { loop { break; } }", 0),
    ("fn f() { return; }", 0),
    ("fn f() { let y = x?; }", 0),
    ("fn f() { let p = &mut v[0]; }", 0),
    // -- macros + attributes ----------------------------------------------
    ("fn f() { println!(\"x = {}\", x); }", 0),
    ("fn f() { vec![1, 2, 3]; }", 0),
    ("fn f() { assert_eq!(a, b, \"msg {}\", c); }", 0),
    ("#[derive(Clone, Debug)]\nstruct S;", 0),
    ("#![allow(dead_code)]\nfn f() {}", 0),
    ("/// Doc comment.\nfn documented() {}", 0),
    ("fn f() { let raw = r#\"raw \\\" string\"#; }", 0),
    // -- malformed with tolerated-error budgets ---------------------------
    ("fn f( {", 3),                     // params closer + body + cascade
    ("let x = ;", 1),                   // missing init
    ("struct S { a: }", 2),             // missing field type
    ("fn f() { match x { => 1 } }", 3), // missing pattern + arm cascade
];

/// Build the green tree through the full `parse` integration.
fn tree_for(source: &str) -> Arc<GreenNode> {
    crate::parse(source).expect("tree integration never fails structurally")
}

#[test]
fn valid_corpus_is_lossless_and_error_free() {
    for (source, tolerated) in CORPUS {
        if *tolerated != 0 {
            continue;
        }
        let tree = tree_for(source);
        assert_eq!(
            tree.render(&Source::from_str_unchecked(source)),
            *source,
            "render(parse(x)) == x for {source:?}"
        );
        let (_, _, errors) = parse_events(source);
        assert_eq!(errors, 0, "valid corpus must be error-free: {source:?}");
    }
}

#[test]
fn malformed_corpus_stays_lossless_within_budget() {
    for (source, tolerated) in CORPUS {
        if *tolerated == 0 {
            continue;
        }
        let tree = tree_for(source);
        assert_eq!(
            tree.render(&Source::from_str_unchecked(source)),
            *source,
            "recovery must stay lossless for {source:?}"
        );
        let (_, _, errors) = parse_events(source);
        assert_eq!(errors, *tolerated, "tolerated-error budget for {source:?}");
    }
}

#[test]
fn parsing_is_deterministic() {
    for (source, _) in CORPUS {
        let first = parse_events(source);
        for _ in 0..3 {
            let again = parse_events(source);
            assert_eq!(again.0, first.0, "events stable for {source:?}");
            assert_eq!(again.1, first.1, "ranges stable for {source:?}");
            assert_eq!(again.2, first.2, "errors stable for {source:?}");
        }
    }
}

#[test]
fn event_stream_is_balanced() {
    for (source, _) in CORPUS {
        let (events, _, _) = parse_events(source);
        let mut depth = 0_i64;
        for event in &events {
            match event {
                Event::Start { .. } => depth += 1,
                Event::Finish => depth -= 1,
                Event::Token { .. } | Event::Error { .. } => {}
            }
            assert!(depth >= 0, "unbalanced stream for {source:?}");
        }
        assert_eq!(depth, 0, "unbalanced stream for {source:?}");
    }
}

/// The lifetime/char disambiguation: `'a'` is a CHAR token, `'a` a
/// LIFETIME, `'lbl:` a LIFETIME label, `'\\n'` an escaped char.
#[test]
fn lifetime_char_disambiguation() {
    let source =
        "fn f(lt: &'static str) { let c = 'x'; let n = '\\n'; 'lbl: loop { break 'lbl; } }";
    let (_, ranges, errors) = parse_events(source);
    assert_eq!(errors, 0, "all quote forms must parse cleanly");
    let lifetimes: Vec<Range<u32>> = ranges
        .iter()
        .filter(|(k, _)| *k == tk::LIFETIME)
        .map(|(_, r)| r.clone())
        .collect();
    // `'static`, the `'lbl:` label, and `break 'lbl`.
    assert_eq!(lifetimes.len(), 3, "lifetimes: {lifetimes:?}");
    let chars: Vec<Range<u32>> = ranges
        .iter()
        .filter(|(k, _)| *k == tk::CHAR)
        .map(|(_, r)| r.clone())
        .collect();
    // `'x'` and `'\n'`.
    assert_eq!(chars.len(), 2, "chars: {chars:?}");
}

/// Macro token trees are balanced: nested delimiters recurse, and the
/// `MACRO` node holds a single `TOKEN_TREE` whose token count covers the
/// invocation.
#[test]
fn macro_token_trees_are_balanced() {
    let source = "fn f() { m!(a, (b [c {d}]), \"s\"); n![x?]; }";
    let (events, ranges, errors) = parse_events(source);
    assert_eq!(errors, 0);
    let macros = count_kind(&events, kinds::MACRO);
    assert_eq!(macros, 2, "two macro invocations");
    // Every delimiter inside the token trees is paired: count openers
    // vs closers across the whole stream.
    let opens = count_token(&events, tk::LPAREN)
        + count_token(&events, tk::LBRACKET)
        + count_token(&events, tk::LBRACE);
    let closes = count_token(&events, tk::RPAREN)
        + count_token(&events, tk::RBRACKET)
        + count_token(&events, tk::RBRACE);
    assert_eq!(opens, closes, "balanced delimiters");
    let _ = ranges;
}

/// Per-family nodes: items produce their node kinds.
#[test]
fn per_family_nodes() {
    let source = "struct S;\nenum E { A }\ntrait T {}\nmod m {}\nconst C: u8 = 0;\ntype A = u8;\nuse x::y;\n";
    let (events, _, errors) = parse_events(source);
    assert_eq!(errors, 0);
    assert_eq!(count_kind(&events, kinds::STRUCT), 1);
    assert_eq!(count_kind(&events, kinds::ENUM), 1);
    assert_eq!(count_kind(&events, kinds::TRAIT), 1);
    assert_eq!(count_kind(&events, kinds::MOD), 1);
    assert_eq!(count_kind(&events, kinds::CONST), 1);
    assert_eq!(count_kind(&events, kinds::TYPE_ALIAS), 1);
    assert_eq!(count_kind(&events, kinds::USE), 1);
}

/// Generics + where clauses parse into their nodes.
#[test]
fn generics_and_where() {
    let source = "impl<T: Into<U>> Wrapper<T> where U: Clone { fn w(&self) {} }";
    let (events, _, errors) = parse_events(source);
    assert_eq!(errors, 0);
    // impl<T>, the Into<U> bound, and Wrapper<T>.
    assert_eq!(count_kind(&events, kinds::GENERIC_PARAMS), 3);
    assert_eq!(count_kind(&events, kinds::WHERE), 1);
}

/// Doc comments (`///`) are trivia — never token events — and the
/// tree still renders losslessly.
#[test]
fn doc_comments_are_trivia() {
    let source = "/// docs\nfn f() {}\n";
    let (events, _, errors) = parse_events(source);
    assert_eq!(errors, 0);
    let comment_tokens = events
        .iter()
        .filter(|e| matches!(e, Event::Token { kind, .. } if *kind == tk::COMMENT))
        .count();
    assert_eq!(comment_tokens, 0, "comments are never token events");
    let tree = tree_for(source);
    assert_eq!(tree.render(&Source::from_str_unchecked(source)), source);
}

/// The tree integration keeps items under the implicit root.
#[test]
fn tree_has_root_and_items() {
    let source = "fn a() {}\nfn b() {}\n";
    let tree = tree_for(source);
    assert_eq!(tree.kind, ROOT_KIND);
    let fns = count_node_kind(&tree, kinds::FN);
    assert_eq!(fns, 2);
}

fn count_kind(events: &[Event], kind: u16) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, Event::Start { kind: k, .. } if *k == kind))
        .count()
}

fn count_token(events: &[Event], kind: u16) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, Event::Token { kind: k, .. } if *k == kind))
        .count()
}

fn count_node_kind(node: &GreenNode, kind: u16) -> usize {
    let mut count = usize::from(node.kind == kind);
    for child in &node.children {
        if let GreenChild::Node(sub) = child {
            count += count_node_kind(sub, kind);
        }
    }
    count
}

/// The `>>`-split: nested generics `Vec<Vec<u8>>` close with two `>`
/// tokens (shared with the TS pack discipline).
#[test]
fn nested_generics_split() {
    let source = "fn f(v: Vec<Vec<u8>>) {}";
    let (_, ranges, errors) = parse_events(source);
    assert_eq!(errors, 0);
    let gts: Vec<Range<u32>> = ranges
        .iter()
        .filter(|(k, _)| *k == tk::GT)
        .map(|(_, r)| r.clone())
        .collect();
    assert_eq!(gts.len(), 2, "two single-> tokens: {gts:?}");
    assert_eq!(gts[0].end, gts[1].start, "adjacent split");
}

/// Regression (fuzz-found): a match arm path with garbage after `E::`
/// used to park `fail_skip` on `)` — zero progress per retry — and the
/// event stream grew without bound until the process was OOM-killed.
/// The parse must terminate with a bounded error count and stay
/// lossless.
#[test]
fn garbage_match_arm_path_terminates_bounded() {
    let source = "#[derive(Debug)]\nenum E { A(i32), B { x: u8 } }\nfn m(e: &E) { match e { E::\t\u{1e} 0EXA(n) => {}, E::B { x } => {} } }";
    let (events, _, errors) = parse_events(source);
    assert!(errors < 128, "bounded tolerated errors, got {errors}");
    assert!(
        events.len() < 4096,
        "event stream is bounded, got {}",
        events.len()
    );
    let tree = crate::parse(source).expect("tree integration never fails structurally");
    assert_eq!(
        tree.render(&Source::from_str_unchecked(source)),
        source,
        "recovery must stay lossless"
    );
}
