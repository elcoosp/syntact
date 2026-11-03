//! The differential driver (issue 017): two independent parsers — the
//! packs-json reference and the descent descriptor engine — must agree
//! on the token shape of every valid input. This is the mechanics the
//! flagship packs reuse for their differential oracles.

use std::ops::Range;
use std::sync::Arc;

use descent::{Desc, Pat, PrattDesc, RuleDesc, green_tree, parse_events};

/// One side's shape: the DFS step list.
pub type Shape = Vec<ShapeStep>;
use runtime_core::tree::{GreenChild, GreenNode};

/// One DFS step of a tree shape: node enter/exit or a non-trivia token.
#[derive(Debug, PartialEq, Eq)]
pub enum ShapeStep {
    /// A node of `kind` opened.
    Enter(u16),
    /// The innermost node closed.
    Exit,
    /// A token of `kind` at `range`.
    Token(u16, Range<u32>),
}

/// The full DFS shape of a green tree, skipping trivia leaves
/// (`trivia_kind`).
#[must_use]
pub fn token_shape(tree: &GreenNode, trivia_kind: u16) -> Vec<ShapeStep> {
    fn walk(node: &GreenNode, trivia: u16, out: &mut Vec<ShapeStep>) {
        out.push(ShapeStep::Enter(node.kind));
        for child in &node.children {
            match child {
                GreenChild::Node(sub) => walk(sub, trivia, out),
                GreenChild::Token { kind, range } => {
                    if *kind != trivia {
                        out.push(ShapeStep::Token(*kind, range.clone()));
                    }
                }
            }
        }
        out.push(ShapeStep::Exit);
    }
    let mut out = Vec::new();
    walk(tree, trivia_kind, &mut out);
    out
}

/// The differential rig: descriptor engine vs reference parser.
pub struct Differential {
    desc: Desc,
    /// The whitespace trivia kind used by both sides' tree integration.
    trivia_kind: u16,
}

/// Structural punctuation kinds of the JSON fixture (mirrors the
/// packs-json token table).
pub mod jk {
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
    /// Whitespace trivia.
    pub const WS: u16 = 14;
    /// Unmatched character sentinel (hand-lexer feed).
    pub const BAD: u16 = 15;
}

impl Differential {
    /// Build the rig with the JSON-shaped descriptor fixture.
    #[must_use]
    pub fn json() -> Self {
        Self {
            desc: json_desc(),
            trivia_kind: jk::WS,
        }
    }

    /// Compare both implementations on `source`. Returns the reference
    /// and engine shapes for inspection; `None` when the engine errored
    /// (the case is malformed — differential testing targets valid docs).
    ///
    /// # Errors
    /// Structural failures of the tree integration surface as `Err`.
    pub fn compare(&self, source: &str) -> Result<Option<(Shape, Shape)>, String> {
        let feed = lex_json(source);
        let run = parse_events(&self.desc, &feed, source);
        if run.errors != 0 {
            return Ok(None);
        }
        let engine = green_tree(
            &run,
            source,
            packs_json::reference::ROOT_KIND,
            self.trivia_kind,
        )?;
        let reference: Arc<GreenNode> = packs_json::parse(source)?;
        let engine_shape = token_shape(&engine, self.trivia_kind);
        let reference_shape = token_shape(&reference, self.trivia_kind);
        Ok(Some((reference_shape, engine_shape)))
    }

    /// Whether both implementations agree on `source`'s shape.
    ///
    /// # Errors
    /// Structural failures of the tree integration surface as `Err`.
    pub fn agrees(&self, source: &str) -> Result<bool, String> {
        Ok(self
            .compare(source)?
            .is_some_and(|(reference, engine)| reference == engine))
    }
}

/// Minimal JSON hand lexer for the feed (same discipline as the packs:
/// clean ranges, trivia excluded, garbage as one-char [`jk::BAD`] tokens).
#[must_use]
pub fn lex_json(source: &str) -> Vec<(u16, Range<u32>)> {
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
                jk::LBRACE
            }
            b'}' => {
                pos += 1;
                jk::RBRACE
            }
            b'[' => {
                pos += 1;
                jk::LBRACK
            }
            b']' => {
                pos += 1;
                jk::RBRACK
            }
            b':' => {
                pos += 1;
                jk::COLON
            }
            b',' => {
                pos += 1;
                jk::COMMA
            }
            b'"' => {
                pos += 1;
                while pos < bytes.len() && bytes[pos] != b'"' {
                    pos += if bytes[pos] == b'\\' { 2 } else { 1 };
                }
                pos = (pos + 1).min(bytes.len());
                3 // kinds::STRING
            }
            b'-' | b'0'..=b'9' => {
                pos += 1;
                while pos < bytes.len()
                    && matches!(bytes[pos], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
                {
                    pos += 1;
                }
                4 // kinds::NUMBER
            }
            _ => {
                if source[pos..].starts_with("true") {
                    pos += 4;
                    5
                } else if source[pos..].starts_with("false") {
                    pos += 5;
                    6
                } else if source[pos..].starts_with("null") {
                    pos += 4;
                    7
                } else {
                    pos += 1;
                    jk::BAD
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

/// `value` alternatives (scalars + containers).
fn value() -> Pat {
    Pat::Alt(vec![
        Pat::Peek {
            tok: jk::LBRACE,
            pat: Box::new(Pat::Rule(0)),
        },
        Pat::Peek {
            tok: jk::LBRACK,
            pat: Box::new(Pat::Rule(1)),
        },
        Pat::Tok(3),
        Pat::Tok(4),
        Pat::Tok(5),
        Pat::Tok(6),
        Pat::Tok(7),
    ])
}

/// The JSON-shaped descriptor fixture (same shape as the descent tests).
#[must_use]
pub fn json_desc() -> Desc {
    Desc {
        rules: vec![
            RuleDesc {
                node_kind: 0, // object
                body: Pat::Seq(vec![
                    Pat::Tok(jk::LBRACE),
                    Pat::Opt(Box::new(Pat::PeekSet {
                        toks: vec![3],
                        pat: Box::new(Pat::Seq(vec![
                            Pat::Rule(2),
                            Pat::Rep(Box::new(Pat::Seq(vec![Pat::Tok(jk::COMMA), Pat::Rule(2)]))),
                        ])),
                    })),
                    Pat::Tok(jk::RBRACE),
                ]),
                sync: vec![jk::COMMA, jk::RBRACE, jk::RBRACK],
                mode_hook: None,
            },
            RuleDesc {
                node_kind: 1, // array
                body: Pat::Seq(vec![
                    Pat::Tok(jk::LBRACK),
                    Pat::Opt(Box::new(Pat::PeekSet {
                        toks: vec![jk::LBRACE, jk::LBRACK, 3, 4, 5, 6, 7],
                        pat: Box::new(Pat::Seq(vec![
                            value(),
                            Pat::Rep(Box::new(Pat::Seq(vec![Pat::Tok(jk::COMMA), value()]))),
                        ])),
                    })),
                    Pat::Tok(jk::RBRACK),
                ]),
                sync: vec![jk::COMMA, jk::RBRACE, jk::RBRACK],
                mode_hook: None,
            },
            RuleDesc {
                node_kind: 2, // pair
                body: Pat::Seq(vec![Pat::Tok(3), Pat::Tok(jk::COLON), value()]),
                sync: vec![jk::COMMA, jk::RBRACE, jk::RBRACK],
                mode_hook: None,
            },
        ],
        exprs: vec![PrattDesc {
            atom: Pat::Tok(4),
            group: None,
            prefix: vec![],
            postfix: vec![],
            infix: vec![],
        }],
        start: Pat::Rep(Box::new(Pat::Alt({
            let Pat::Alt(mut alts) = value() else {
                unreachable!("value() is an Alt")
            };
            alts.push(Pat::Recover);
            alts
        }))),
    }
}
