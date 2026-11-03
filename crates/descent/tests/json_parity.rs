//! Descriptor parity suites (issue 023 TDD): a JSON-shaped descriptor
//! grammar parsed by the descent engine must agree with the hand-built
//! reference parser (`packs_json::reference`) on every valid document,
//! and the stream invariants (balance, single root after wrap, full
//! trivia coverage, progress on error) must hold on every input.

use std::sync::Arc;

use descent::{
    Desc, InfixDesc, Pat, PrattDesc, RuleDesc, UnaryDesc, green_tree, parse_events,
    stream_is_balanced,
};
use packs_json::kinds;
use pretty_assertions::assert_eq;
use runtime_core::ERROR_KIND;
use runtime_core::input::Source;
use runtime_core::tree::{GreenChild, GreenNode};

// ---------------------------------------------------------------------------
// JSON token table + hand lexer fixture (clean feed for the engine)
// ---------------------------------------------------------------------------

/// Structural punctuation token kinds — dense continuation of the value
/// kinds, mirroring `packs_json::reference::token_kinds`.
mod tk {
    /// `{`
    pub const LBRACE: u16 = 8;
    /// `}`
    pub const RBRACE: u16 = 9;
    /// `[`
    pub const LBRACK: u16 = 10;
    /// `]`
    pub const RBRACK: u16 = 11;
    /// `:`
    pub const COLON: u16 = 12;
    /// `,`
    pub const COMMA: u16 = 13;
    /// whitespace trivia — never a feed token (ADR-008); flows through
    /// trivia records computed from feed gaps.
    pub const WS: u16 = 14;
}

/// The unmatched-character token kind (the feed's error sentinel; the
/// engine recovers it into ERROR regions).
const BAD: u16 = 15;

/// Minimal JSON lexer fixture: clean `(kind, range)` feed, garbage chars
/// as one-char [`BAD`] tokens (progress), trivia excluded.
fn lex(source: &str) -> Vec<(u16, std::ops::Range<u32>)> {
    let bytes = source.as_bytes();
    let mut feed = Vec::new();
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
                tk::LBRACE
            }
            b'}' => {
                pos += 1;
                tk::RBRACE
            }
            b'[' => {
                pos += 1;
                tk::LBRACK
            }
            b']' => {
                pos += 1;
                tk::RBRACK
            }
            b':' => {
                pos += 1;
                tk::COLON
            }
            b',' => {
                pos += 1;
                tk::COMMA
            }
            b'"' => {
                pos += 1;
                while pos < bytes.len() && bytes[pos] != b'"' {
                    pos += if bytes[pos] == b'\\' { 2 } else { 1 };
                }
                pos = (pos + 1).min(bytes.len());
                kinds::STRING
            }
            b'-' | b'0'..=b'9' => {
                pos += 1;
                while pos < bytes.len()
                    && matches!(bytes[pos], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
                {
                    pos += 1;
                }
                kinds::NUMBER
            }
            _ => {
                if source[pos..].starts_with("true") {
                    pos += 4;
                    kinds::TRUE
                } else if source[pos..].starts_with("false") {
                    pos += 5;
                    kinds::FALSE
                } else if source[pos..].starts_with("null") {
                    pos += 4;
                    kinds::NULL
                } else {
                    pos += 1;
                    BAD
                }
            }
        };
        feed.push((
            kind,
            u32::try_from(start).expect("small")..u32::try_from(pos).expect("small"),
        ));
    }
    feed
}

// ---------------------------------------------------------------------------
// The JSON-shaped descriptor grammar (kinds mirror packs_json::kinds)
// ---------------------------------------------------------------------------

/// `value` alternatives (shared by document, array and pair contexts).
fn value() -> Pat {
    Pat::Alt(vec![
        Pat::Peek {
            tok: tk::LBRACE,
            pat: Box::new(Pat::Rule(0)),
        },
        Pat::Peek {
            tok: tk::LBRACK,
            pat: Box::new(Pat::Rule(1)),
        },
        Pat::Tok(kinds::STRING),
        Pat::Tok(kinds::NUMBER),
        Pat::Tok(kinds::TRUE),
        Pat::Tok(kinds::FALSE),
        Pat::Tok(kinds::NULL),
    ])
}

/// The FIRST set of `value` — drives the array-element list guard.
fn value_starts() -> Vec<u16> {
    vec![
        tk::LBRACE,
        tk::LBRACK,
        kinds::STRING,
        kinds::NUMBER,
        kinds::TRUE,
        kinds::FALSE,
        kinds::NULL,
    ]
}

/// The descriptor grammar for JSON. Shape contract (issue 019): value
/// leaves are bare tokens; OBJECT/ARRAY/PAIR are the only nodes.
fn json_desc() -> Desc {
    Desc {
        rules: vec![
            // 0: object = `{` (pair (`,` pair)*)? `}` — pair starts with
            // STRING, so the list is FIRST-guarded and `{}` never
            // speculatively enters the self-recovering pair rule.
            RuleDesc {
                node_kind: kinds::OBJECT,
                body: Pat::Seq(vec![
                    Pat::Tok(tk::LBRACE),
                    Pat::Opt(Box::new(Pat::PeekSet {
                        toks: vec![kinds::STRING],
                        pat: Box::new(Pat::Seq(vec![
                            Pat::Rule(2),
                            Pat::Rep(Box::new(Pat::Seq(vec![Pat::Tok(tk::COMMA), Pat::Rule(2)]))),
                        ])),
                    })),
                    Pat::Tok(tk::RBRACE),
                ]),
                sync: vec![tk::COMMA, tk::RBRACE, tk::RBRACK],
                mode_hook: None,
            },
            // 1: array = `[` (value (`,` value)*)? `]` — guarded by the
            // value FIRST set.
            RuleDesc {
                node_kind: kinds::ARRAY,
                body: Pat::Seq(vec![
                    Pat::Tok(tk::LBRACK),
                    Pat::Opt(Box::new(Pat::PeekSet {
                        toks: value_starts(),
                        pat: Box::new(Pat::Seq(vec![
                            value(),
                            Pat::Rep(Box::new(Pat::Seq(vec![Pat::Tok(tk::COMMA), value()]))),
                        ])),
                    })),
                    Pat::Tok(tk::RBRACK),
                ]),
                sync: vec![tk::COMMA, tk::RBRACE, tk::RBRACK],
                mode_hook: None,
            },
            // 2: pair = STRING `:` value
            RuleDesc {
                node_kind: kinds::PAIR,
                body: Pat::Seq(vec![Pat::Tok(kinds::STRING), Pat::Tok(tk::COLON), value()]),
                sync: vec![tk::COMMA, tk::RBRACE, tk::RBRACK],
                mode_hook: None,
            },
        ],
        exprs: vec![],
        // document = (value)* with recovery as the last resort — no
        // wrapping node (the caller's root wrap is the single root).
        start: Pat::Rep(Box::new(Pat::Alt({
            let Pat::Alt(mut alts) = value() else {
                unreachable!("value() is an Alt")
            };
            alts.push(Pat::Recover);
            alts
        }))),
    }
}

// ---------------------------------------------------------------------------
// Tree shape extraction (structural parity basis)
// ---------------------------------------------------------------------------

/// One DFS step of a tree shape: node enter/exit or a non-trivia token.
#[derive(Debug, PartialEq, Eq)]
enum ShapeStep {
    /// A node of `kind` opened.
    Enter(u16),
    /// The innermost node closed.
    Exit,
    /// A token of `kind` at `range`.
    Token(u16, std::ops::Range<u32>),
}

/// The full DFS shape of a green tree, trivia leaves (kind 14) skipped —
/// trivia classification is pack policy, parity here is structure.
fn shape(tree: &GreenNode) -> Vec<ShapeStep> {
    fn walk(node: &GreenNode, out: &mut Vec<ShapeStep>) {
        out.push(ShapeStep::Enter(node.kind));
        for child in &node.children {
            match child {
                GreenChild::Node(sub) => walk(sub, out),
                GreenChild::Token { kind, range } => {
                    if *kind != tk::WS {
                        out.push(ShapeStep::Token(*kind, range.clone()));
                    }
                }
            }
        }
        out.push(ShapeStep::Exit);
    }
    let mut out = Vec::new();
    walk(tree, &mut out);
    out
}

fn engine_tree(source: &str) -> Arc<GreenNode> {
    let feed = lex(source);
    let run = parse_events(&json_desc(), &feed, source);
    green_tree(&run, source, packs_json::reference::ROOT_KIND, tk::WS)
        .expect("tree integration never fails structurally")
}

// ---------------------------------------------------------------------------
// Suites
// ---------------------------------------------------------------------------

const VALID_CORPUS: &[&str] = &[
    "{}",
    "[]",
    r#"{"a": 1}"#,
    "[1, 2, 3]",
    r#"{"name": "syntact", "version": 1, "ok": true, "bits": [true, false, null]}"#,
    "3.14e-2",
    "-17",
    r#""str with \u00e9 escape""#,
    "  [ 1 , 2 ]  ",
    r#"{"a": [1, 2, {"b": null}], "c": true}"#,
    "1 2",
    "",
];

#[test]
fn descriptor_json_matches_reference_shape_on_valid_corpus() {
    for source in VALID_CORPUS {
        let feed = lex(source);
        let run = parse_events(&json_desc(), &feed, source);
        assert_eq!(run.errors, 0, "valid corpus must be error-free: {source:?}");
        let ours = engine_tree(source);
        let theirs: Arc<GreenNode> = packs_json::parse(source).expect("reference parses");
        assert_eq!(shape(&ours), shape(&theirs), "shape parity on {source:?}");
        // Byte-faithfulness (trivia coverage) on the engine path too.
        assert_eq!(
            ours.render(&Source::from_str_unchecked(source)),
            *source,
            "render(parse(x)) == x for {source:?}"
        );
    }
}

#[test]
fn stream_invariants_hold_on_valid_and_malformed() {
    let corpus: &[&str] = &[
        "{}",
        r#"{"a": [1, 2, {"b": null}], "c": true}"#,
        "{\"a\": }",
        "[1, 2",
        "tru",
        "@#$",
        "",
    ];
    for source in corpus {
        let feed = lex(source);
        let run = parse_events(&json_desc(), &feed, source);
        assert!(
            stream_is_balanced(&run, source, packs_json::reference::ROOT_KIND),
            "balanced + partitioning on {source:?}"
        );
    }
}

#[test]
fn malformed_inputs_error_and_stay_lossless() {
    let cases: &[&str] = &["{\"a\": }", "[1, 2", "tru", "@#$"];
    for source in cases {
        let feed = lex(source);
        let run = parse_events(&json_desc(), &feed, source);
        assert!(run.errors >= 1, "at least one error on {source:?}");
        assert!(
            run.events
                .iter()
                .any(|e| matches!(e, runtime_core::events::Event::Error { .. })),
            "error event on {source:?}"
        );
        let enters: Vec<u16> = run
            .events
            .iter()
            .filter_map(|e| match e {
                runtime_core::events::Event::Start { kind, .. } => Some(*kind),
                _ => None,
            })
            .collect();
        assert!(enters.contains(&ERROR_KIND), "error node on {source:?}");
        // Lossless through the tree integration.
        let tree = engine_tree(source);
        assert_eq!(
            tree.render(&Source::from_str_unchecked(source)),
            *source,
            "lossless on {source:?}"
        );
    }
}

#[test]
fn recovery_always_makes_progress() {
    // Pathological garbage: every error consumes a token, the run
    // terminates, and the feed is fully accounted for.
    let source = "@!#?@! #garbage !!!!";
    let feed = lex(source);
    let run = parse_events(&json_desc(), &feed, source);
    assert_eq!(
        run.errors as usize,
        feed.len(),
        "one error per garbage token"
    );
    assert!(
        stream_is_balanced(&run, source, packs_json::reference::ROOT_KIND),
        "balanced + partitioning on garbage"
    );
}

#[test]
fn mode_hooks_are_pinned_in_the_descriptor() {
    // A rule pinning mode 3 records its crossings.
    let desc = Desc {
        rules: vec![RuleDesc {
            node_kind: 7,
            body: Pat::Seq(vec![Pat::Tok(1), Pat::Tok(2)]),
            sync: vec![],
            mode_hook: Some(3),
        }],
        exprs: vec![],
        start: Pat::Rule(0),
    };
    let source = "ab";
    let feed = vec![(1_u16, 0_u32..1), (2_u16, 1..2)];
    let run = parse_events(&desc, &feed, source);
    assert_eq!(
        run.modes,
        vec![
            descent::engine::ModeCrossing {
                mode: 3,
                enter: true,
                at: 0
            },
            descent::engine::ModeCrossing {
                mode: 3,
                enter: false,
                at: 2
            },
        ]
    );
}

/// The Pratt descriptor surface stays importable from the crate root
/// (the expression backend shares the descriptor data model).
#[test]
fn pratt_descriptor_surface_is_reachable() {
    let table = PrattDesc {
        atom: Pat::Tok(1),
        group: None,
        prefix: vec![UnaryDesc {
            token: 2,
            node: 3,
            bp: 6,
        }],
        postfix: vec![UnaryDesc {
            token: 4,
            node: 5,
            bp: 7,
        }],
        infix: vec![InfixDesc {
            token: 6,
            lbp: 1,
            rbp: 2,
            node: 7,
        }],
    };
    assert_eq!(table.prefix[0].bp, 6);
    assert_eq!(table.infix[0].rbp, 2);
}
