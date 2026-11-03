//! Pratt suites (issue 023 TDD): golden precedence shapes (binding
//! powers, left/right associativity, prefix/postfix mix-ins) plus the
//! Pratt-vs-brute-force differential — the climbed tree must evaluate to
//! the same value a shunting-yard evaluator computes, over a seeded
//! random corpus.

use descent::{Desc, InfixDesc, Pat, PrattDesc, RuleDesc, UnaryDesc, green_tree, parse_events};
use pretty_assertions::assert_eq;
use runtime_core::tree::{GreenChild, GreenNode};

// Token kinds (test-local dense table).
const INT: u16 = 0;
const PLUS: u16 = 1;
const MINUS: u16 = 2;
const STAR: u16 = 3;
const SLASH: u16 = 4;
const CARET: u16 = 5;
const LPAREN: u16 = 6;
const RPAREN: u16 = 7;
const BANG: u16 = 8;
const TILDE: u16 = 9;
// Node kinds.
const NUM: u16 = 20;
const ADD: u16 = 21;
const SUB: u16 = 22;
const MUL: u16 = 23;
const DIV: u16 = 24;
const POW: u16 = 25;
const NEG: u16 = 26;
const FACT: u16 = 27;
const INV: u16 = 28;
const EXPR: u16 = 29;

/// The arithmetic Pratt grammar: `* /` > `+ -` (left-assoc), `^`
/// right-assoc, prefix `- ~`, postfix `!`, transparent paren groups.
fn arith_desc() -> Desc {
    let expr = PrattDesc {
        atom: Pat::Node(NUM, Box::new(Pat::Tok(INT))),
        group: Some((LPAREN, RPAREN)),
        prefix: vec![
            UnaryDesc {
                token: MINUS,
                node: NEG,
                bp: 6,
            },
            UnaryDesc {
                token: TILDE,
                node: INV,
                bp: 6,
            },
        ],
        postfix: vec![UnaryDesc {
            token: BANG,
            node: FACT,
            bp: 7,
        }],
        infix: vec![
            InfixDesc {
                token: PLUS,
                lbp: 1,
                rbp: 2,
                node: ADD,
            },
            InfixDesc {
                token: MINUS,
                lbp: 1,
                rbp: 2,
                node: SUB,
            },
            InfixDesc {
                token: STAR,
                lbp: 3,
                rbp: 4,
                node: MUL,
            },
            InfixDesc {
                token: SLASH,
                lbp: 3,
                rbp: 4,
                node: DIV,
            },
            InfixDesc {
                token: CARET,
                lbp: 8,
                rbp: 8,
                node: POW,
            },
        ],
    };
    Desc {
        rules: vec![RuleDesc {
            node_kind: EXPR,
            body: Pat::Expr(0),
            sync: vec![],
            mode_hook: None,
        }],
        exprs: vec![expr],
        start: Pat::Rep(Box::new(Pat::Rule(0))),
    }
}

/// Lex an arithmetic expression fixture.
fn lex(source: &str) -> Vec<(u16, std::ops::Range<u32>)> {
    let bytes = source.as_bytes();
    let mut feed = Vec::new();
    let mut pos = 0_usize;
    while pos < bytes.len() {
        let start = pos;
        let kind = match bytes[pos] {
            b' ' => {
                pos += 1;
                continue;
            }
            b'0'..=b'9' => {
                while pos < bytes.len() && bytes[pos].is_ascii_digit() {
                    pos += 1;
                }
                INT
            }
            b'+' => {
                pos += 1;
                PLUS
            }
            b'-' => {
                pos += 1;
                MINUS
            }
            b'*' => {
                pos += 1;
                STAR
            }
            b'/' => {
                pos += 1;
                SLASH
            }
            b'^' => {
                pos += 1;
                CARET
            }
            b'(' => {
                pos += 1;
                LPAREN
            }
            b')' => {
                pos += 1;
                RPAREN
            }
            b'!' => {
                pos += 1;
                BANG
            }
            b'~' => {
                pos += 1;
                TILDE
            }
            _ => panic!("fixture lexer covers ascii arithmetic only"),
        };
        feed.push((
            kind,
            u32::try_from(start).expect("small")..u32::try_from(pos).expect("small"),
        ));
    }
    feed
}

/// Parse one expression into its tree.
fn expr_tree(source: &str) -> std::sync::Arc<GreenNode> {
    let feed = lex(source);
    let run = parse_events(&arith_desc(), &feed, source);
    assert_eq!(run.errors, 0, "golden fixtures are error-free: {source:?}");
    green_tree(&run, source, 99, 98).expect("tree builds")
}

/// Evaluate the (single) expression of a golden fixture.
fn expr_value(source: &str) -> i64 {
    let tree = expr_tree(source);
    let expr = match &tree.children[0] {
        GreenChild::Node(sub) => sub.clone(),
        GreenChild::Token { .. } => panic!("expression document"),
    };
    eval(&expr, source)
}

/// Evaluate a parse tree with the test-local semantics.
fn eval(node: &GreenNode, source: &str) -> i64 {
    let kids: Vec<&GreenChild> = node.children.iter().collect();
    // Operand value: a node, or the INT token (leading trivia tokens may
    // precede it inside the node — skip them).
    let value = |idx: usize| -> i64 {
        match &kids[idx] {
            GreenChild::Token { kind, range } if *kind == INT => source
                [range.start as usize..range.end as usize]
                .parse()
                .expect("int literal"),
            GreenChild::Token { .. } => panic!("operand token must be INT"),
            GreenChild::Node(sub) => eval(sub, source),
        }
    };
    let ops: Vec<i64> = (0..kids.len())
        .filter(|i| !matches!(kids[*i], GreenChild::Token { .. }))
        .map(value)
        .collect();
    let _ = &kids;
    match node.kind {
        NUM => (0..kids.len())
            .find_map(|i| match kids[i] {
                GreenChild::Token { kind, range } if *kind == INT => Some(
                    source[range.start as usize..range.end as usize]
                        .parse::<i64>()
                        .expect("int literal"),
                ),
                _ => None,
            })
            .unwrap_or_else(|| panic!("NUM node carries an INT token")),
        ADD => ops.iter().copied().sum(),
        SUB => ops[0] - ops[1],
        MUL => ops.iter().product(),
        DIV => ops[0] / ops[1].max(1),
        POW => ops[0].pow(u32::try_from(ops[1].max(0)).unwrap_or(0).min(20)),
        NEG => -ops[0],
        INV => !ops[0],
        FACT => (2..=ops[0].max(1)).product::<i64>().max(1),
        EXPR => ops[0],
        _ => panic!("evaluator covers the fixture kinds only, got {}", node.kind),
    }
}

#[test]
fn precedence_golden_shapes() {
    // `*` binds tighter than `+`: 1 + 2 * 3 == 1 + (2 * 3)
    assert_eq!(expr_value("1 + 2 * 3"), 7);
    // and on the left: 1 * 2 + 3 == (1 * 2) + 3
    assert_eq!(expr_value("1 * 2 + 3"), 5);
    // `^` right-assoc: 2 ^ 3 ^ 2 == 2 ^ (3 ^ 2) == 512
    assert_eq!(expr_value("2 ^ 3 ^ 2"), 512);
    // `+ -` left-assoc: 1 - 2 - 3 == (1 - 2) - 3
    assert_eq!(expr_value("1 - 2 - 3"), -4);
    // parens override: (1 + 2) * 3 == 9
    assert_eq!(expr_value("(1 + 2) * 3"), 9);
}

#[test]
fn prefix_and_postfix_mix_ins() {
    // Prefix minus binds tighter than +: -1 + 2 == (-1) + 2
    assert_eq!(expr_value("-1 + 2"), 1);
    // Nested prefix: --4 == 4
    assert_eq!(expr_value("--4"), 4);
    // Prefix on a parenthesized operand: -(2 + 3) == -5
    assert_eq!(expr_value("-(2 + 3)"), -5);
    // Postfix binds tightest: 2! ^ 2 == (2!) ^ 2 == 4
    assert_eq!(expr_value("2! ^ 2"), 4);
    // Postfix after prefix: -3! == -(3!) == -6
    assert_eq!(expr_value("-3!"), -6);
    // Bitwise-not mix-in: ~0 == -1
    assert_eq!(expr_value("~0"), -1);
    // Mixed chain: ~2 + 3! * -1 == ~2 + (3! * (-1)) == -3 + (-6) —
    // wait: ~2 == -3, 3! * -1 == -6, sum == -9.
    assert_eq!(expr_value("~2 + 3! * -1"), -9);
}

/// Random expression token stream: ints, + - * /, parens, unary minus.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Tok {
    /// An integer literal.
    Int(i64),
    /// A binary operator.
    Op(char),
    /// Prefix unary minus.
    UMinus,
    /// Open paren.
    LParen,
    /// Close paren.
    RParen,
}

fn xorshift(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

/// Random expression generator (valid by construction).
fn gen_toks(state: &mut u64, depth: u32, out: &mut Vec<Tok>) {
    let r = xorshift(state);
    if depth == 0 || r % 3 == 0 {
        if r % 5 == 0 {
            out.push(Tok::UMinus);
        }
        out.push(Tok::Int(
            i64::try_from(xorshift(state) % 9 + 1).expect("small"),
        ));
        return;
    }
    match r % 3 {
        0 | 1 => {
            gen_toks(state, depth - 1, out);
            out.push(match r % 4 {
                0 => Tok::Op('+'),
                1 => Tok::Op('-'),
                2 => Tok::Op('*'),
                _ => Tok::Op('/'),
            });
            gen_toks(state, depth - 1, out);
        }
        _ => {
            out.push(Tok::LParen);
            gen_toks(state, depth - 1, out);
            out.push(Tok::RParen);
        }
    }
}

/// Render tokens to source text (the Pratt path lexes this back).
fn render(toks: &[Tok]) -> String {
    let mut s = String::new();
    for tok in toks {
        match tok {
            Tok::Int(n) => s.push_str(&n.to_string()),
            Tok::Op(op) => s.push(*op),
            Tok::UMinus => s.push('-'),
            Tok::LParen => s.push('('),
            Tok::RParen => s.push(')'),
        }
    }
    s
}

/// Shunting-yard over the token stream: unary minus = right-assoc
/// prefix op at precedence 4; `* /` = 3; `+ -` = 2. Returns the RPN value.
fn shunting_yard(toks: &[Tok]) -> i64 {
    let mut rpn: Vec<Tok> = Vec::new();
    let mut ops: Vec<Tok> = Vec::new();
    for tok in toks {
        match tok {
            Tok::Int(_) => rpn.push(*tok),
            Tok::UMinus => ops.push(Tok::UMinus),
            Tok::Op(op) => {
                let prec = if *op == '*' || *op == '/' { 3 } else { 2 };
                while let Some(top) = ops.last() {
                    let top_prec = match top {
                        Tok::UMinus => 4,
                        Tok::Op(t) if *t == '*' || *t == '/' => 3,
                        Tok::Op(_) => 2,
                        _ => 0,
                    };
                    if top_prec >= prec {
                        rpn.push(ops.pop().expect("checked"));
                    } else {
                        break;
                    }
                }
                ops.push(*tok);
            }
            Tok::LParen => ops.push(Tok::LParen),
            Tok::RParen => {
                while let Some(top) = ops.pop() {
                    if top == Tok::LParen {
                        break;
                    }
                    rpn.push(top);
                }
            }
        }
    }
    while let Some(top) = ops.pop() {
        rpn.push(top);
    }
    let mut stack: Vec<i64> = Vec::new();
    for tok in rpn {
        match tok {
            Tok::Int(n) => stack.push(n),
            Tok::UMinus => {
                let v = stack.pop().expect("unary operand");
                stack.push(-v);
            }
            Tok::Op(op) => {
                let b = stack.pop().expect("rhs");
                let a = stack.pop().expect("lhs");
                stack.push(match op {
                    '+' => a + b,
                    '-' => a - b,
                    '*' => a * b,
                    _ => a / b.max(1),
                });
            }
            _ => panic!("no parens in RPN"),
        }
    }
    stack.pop().expect("one result")
}

#[test]
fn pratt_agrees_with_shunting_yard_brute_force() {
    // Seeded xorshift corpus; two INDEPENDENT evaluators (Pratt-climbed
    // tree vs shunting-yard RPN) must agree on every case.
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    for _case in 0..200 {
        let mut toks = Vec::new();
        gen_toks(&mut state, 3, &mut toks);
        let source = render(&toks);
        let feed = lex(&source);
        let run = parse_events(&arith_desc(), &feed, &source);
        assert_eq!(run.errors, 0, "generated corpus is error-free: {source:?}");
        let tree = green_tree(&run, &source, 99, 98).expect("tree builds");
        let expr = match &tree.children[0] {
            GreenChild::Node(sub) => sub.clone(),
            GreenChild::Token { .. } => panic!("expression document"),
        };
        assert_eq!(
            eval(&expr, &source),
            shunting_yard(&toks),
            "differential on {source:?}"
        );
    }
}
