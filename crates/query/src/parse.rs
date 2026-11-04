//! Query text parsing (issue 031): the `.scm` subset into a typed
//! [`QuerySpec`] — every error carries the byte span of the offending
//! text.
//!
//! Subset: `(node-kind child… @capture quant)` patterns (arbitrary
//! nesting), `_` wildcards, quantifiers `? * +`, and `#eq?` predicates
//! attached to the preceding pattern.

use std::ops::Range;

/// Quantifier semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quant {
    /// Exactly one (no marker).
    One,
    /// `?` — zero or one.
    ZeroOne,
    /// `*` — zero or more.
    ZeroPlus,
    /// `+` — one or more.
    OnePlus,
}

/// A pattern node (recursive; children are the direct sub-patterns).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QNode {
    /// The node kind name (`None` for the `_` wildcard).
    pub kind: Option<String>,
    /// The capture attached to this node (`@name`), if any.
    pub capture: Option<String>,
    /// The quantifier.
    pub quant: Quant,
    /// The text span of this node (for diagnostics).
    pub span: Range<u32>,
    /// The direct children.
    pub children: Vec<QNode>,
}

/// One pattern with its attached predicates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternDef {
    /// The root node.
    pub root: QNode,
    /// `#eq? @cap "value"` predicates, in order.
    pub predicates: Vec<PredicateDef>,
    /// The whole pattern's text span.
    pub span: Range<u32>,
}

/// `(#eq? @cap "value")`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PredicateDef {
    /// The capture name (without `@`).
    pub capture: String,
    /// The literal to compare against.
    pub value: String,
    /// The predicate's text span.
    pub span: Range<u32>,
}

/// The parsed query.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QuerySpec {
    /// Patterns in source order.
    pub patterns: Vec<PatternDef>,
}

/// A query error with the offending span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryError {
    /// Byte span of the offending text.
    pub span: Range<u32>,
    /// What went wrong.
    pub message: String,
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at byte {}", self.message, self.span.start)
    }
}

impl std::error::Error for QueryError {}

/// Token of the query text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Tok {
    kind: TokKind,
    span: Range<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TokKind {
    LParen,
    RParen,
    Wildcard,
    Capture(String),
    Hash(String),
    Str(String),
    Ident(String),
    Quant(Quant),
}

fn lex(text: &str) -> Result<Vec<Tok>, QueryError> {
    // (kept small: the arms delegate to `lex_atom`)
    let bytes = text.as_bytes();
    let mut toks = Vec::new();
    let mut pos = 0_usize;
    while pos < bytes.len() {
        let start = pos;
        let Some((kind, len)) = lex_atom(text, pos)? else {
            // whitespace
            while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }
            if pos == start {
                pos += 1;
                return Err(QueryError {
                    span: span(start, pos),
                    message: format!("unexpected character `{}`", bytes[start] as char),
                });
            }
            continue;
        };
        pos += len;
        toks.push(Tok {
            kind,
            span: span(start, pos),
        });
    }
    Ok(toks)
}

/// One query token at `pos`; `None` for whitespace.
fn lex_atom(text: &str, pos: usize) -> Result<Option<(TokKind, usize)>, QueryError> {
    let bytes = text.as_bytes();
    let Some(b) = bytes.get(pos).copied() else {
        return Ok(None);
    };
    let one = |kind| Some((kind, 1_usize));
    // Two-char sequences first? None in this grammar — single chars below.
    match b {
        b'(' => Ok(one(TokKind::LParen)),
        b')' => Ok(one(TokKind::RParen)),
        b'?' => Ok(one(TokKind::Quant(Quant::ZeroOne))),
        b'*' => Ok(one(TokKind::Quant(Quant::ZeroPlus))),
        b'+' => Ok(one(TokKind::Quant(Quant::OnePlus))),
        b'"' => {
            let mut idx = pos + 1;
            let mut value = String::new();
            let mut closed = false;
            while idx < bytes.len() {
                if bytes[idx] == b'"' {
                    idx += 1;
                    closed = true;
                    break;
                }
                if bytes[idx] == b'\\' && idx + 1 < bytes.len() {
                    value.push(bytes[idx + 1] as char);
                    idx += 2;
                    continue;
                }
                value.push(bytes[idx] as char);
                idx += 1;
            }
            if !closed {
                return Err(QueryError {
                    span: span(pos, idx),
                    message: "unterminated string literal".into(),
                });
            }
            Ok(Some((TokKind::Str(value), idx - pos)))
        }
        b'@' | b'#' => {
            let mut idx = pos + 1;
            while idx < bytes.len()
                && (bytes[idx].is_ascii_alphanumeric() || matches!(bytes[idx], b'_' | b'-' | b'?'))
            {
                idx += 1;
            }
            if idx == pos + 1 {
                return Err(QueryError {
                    span: span(pos, idx),
                    message: format!("expected a name after `{}`", b as char),
                });
            }
            let name = text[pos + 1..idx].to_owned();
            let kind = if b == b'@' {
                TokKind::Capture(name)
            } else {
                TokKind::Hash(name)
            };
            Ok(Some((kind, idx - pos)))
        }
        _ if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' => {
            let mut idx = pos + 1;
            while idx < bytes.len()
                && (bytes[idx].is_ascii_alphanumeric() || matches!(bytes[idx], b'_' | b'-'))
            {
                idx += 1;
            }
            let word = &text[pos..idx];
            let kind = if word == "_" {
                TokKind::Wildcard
            } else {
                TokKind::Ident(word.to_owned())
            };
            Ok(Some((kind, idx - pos)))
        }
        _ if b.is_ascii_whitespace() => Ok(None),
        _ => Err(QueryError {
            span: span(pos, pos + 1),
            message: format!("unexpected character `{}`", b as char),
        }),
    }
}

fn span(start: usize, end: usize) -> Range<u32> {
    u32::try_from(start).expect("offset fits u32")..u32::try_from(end).expect("offset fits u32")
}

/// Parse the query text.
///
/// # Errors
/// [`QueryError`] with the offending span on any syntax or shape error.
pub fn parse_query(text: &str) -> Result<QuerySpec, QueryError> {
    let toks = lex(text)?;
    let mut idx = 0_usize;
    let mut spec = QuerySpec::default();
    while idx < toks.len() {
        let Some(tok) = toks.get(idx) else {
            break;
        };
        if !matches!(tok.kind, TokKind::LParen) {
            return Err(QueryError {
                span: tok.span.clone(),
                message: "expected a `(` pattern".into(),
            });
        }
        let pattern_start = tok.span.start;
        // `((a) @x (b) @y …)` — a grouping root holds several root-level
        // patterns (tree-sitter shape); each becomes its own pattern and
        // the trailing predicates attach to the last.
        let grouped = matches!(toks.get(idx + 1).map(|t| &t.kind), Some(TokKind::LParen));
        let mut group_roots: Vec<QNode> = Vec::new();
        let (root, node_preds);
        if grouped {
            idx += 1; // outer (
            loop {
                match toks.get(idx).map(|t| &t.kind) {
                    Some(TokKind::LParen) => {
                        // A predicate S-expr ends the group.
                        if matches!(toks.get(idx + 1).map(|t| &t.kind), Some(TokKind::Hash(_))) {
                            break;
                        }
                        let (member, _) = parse_node(&toks, &mut idx)?;
                        group_roots.push(member);
                    }
                    Some(TokKind::RParen) => break, // the caller consumes it
                    other => {
                        return Err(QueryError {
                            span: tok.span.clone(),
                            message: format!("unexpected {other:?} in pattern group"),
                        });
                    }
                }
            }
            if group_roots.is_empty() {
                return Err(QueryError {
                    span: tok.span.clone(),
                    message: "empty pattern group".into(),
                });
            }
            root = group_roots.pop().expect("checked");
            node_preds = Vec::new();
            let _ = &node_preds;
        } else {
            let (node, preds) = parse_node(&toks, &mut idx)?;
            root = node;
            node_preds = preds;
        }
        let mut predicates = Vec::new();
        while let Some(tok) = toks.get(idx) {
            if !matches!(tok.kind, TokKind::LParen) {
                break; // the next pattern
            }
            // Peek the S-expr: `(#name ...)` → a predicate.
            let is_pred = matches!(toks.get(idx + 1).map(|t| &t.kind), Some(TokKind::Hash(_)));
            if !is_pred {
                break;
            }
            let pred = parse_predicate(&toks, &mut idx)?;
            predicates.push(pred);
        }
        // A grouped pattern's outer `)` follows the predicates.
        if grouped {
            if !matches!(toks.get(idx).map(|t| &t.kind), Some(TokKind::RParen)) {
                return Err(QueryError {
                    span: tok.span.clone(),
                    message: "unterminated pattern group".into(),
                });
            }
            idx += 1;
        }
        predicates.extend(node_preds);
        let pattern_end = root.span.end;
        // The earlier group members become their own patterns.
        for mut member in group_roots {
            member.span = member.span.clone();
            let span = member.span.clone();
            spec.patterns.push(PatternDef {
                root: member,
                predicates: Vec::new(),
                span,
            });
        }
        spec.patterns.push(PatternDef {
            root,
            predicates,
            span: pattern_start..pattern_end,
        });
    }
    Ok(spec)
}

fn parse_node(toks: &[Tok], idx: &mut usize) -> Result<(QNode, Vec<PredicateDef>), QueryError> {
    let open_span = toks[*idx].span.clone();
    *idx += 1;
    let Some(tok) = toks.get(*idx) else {
        return Err(QueryError {
            span: open_span,
            message: "unterminated node".into(),
        });
    };
    let kind_name = match &tok.kind {
        TokKind::Ident(name) => Some(name.clone()),
        TokKind::Wildcard => None,
        other => {
            return Err(QueryError {
                span: tok.span.clone(),
                message: format!("expected a node kind or `_`, got {other:?}"),
            });
        }
    };
    let mut node = QNode {
        kind: kind_name,
        capture: None,
        quant: Quant::One,
        span: open_span.start..tok.span.end,
        children: Vec::new(),
    };
    let mut node_preds: Vec<PredicateDef> = Vec::new();
    *idx += 1;
    // Children until the closing paren.
    let mut closed = false;
    while let Some(tok) = toks.get(*idx) {
        match &tok.kind {
            TokKind::RParen => {
                node.span = node.span.start..tok.span.end;
                *idx += 1;
                closed = true;
                break;
            }
            TokKind::LParen => {
                // A predicate S-expr (`(#eq? …)`) rides with this node.
                if matches!(toks.get(*idx + 1).map(|t| &t.kind), Some(TokKind::Hash(_))) {
                    let pred = parse_predicate(toks, idx)?;
                    node_preds.push(pred);
                    continue;
                }
                let (child, child_preds) = parse_node(toks, idx)?;
                node_preds.extend(child_preds);
                node.children.push(child);
            }
            TokKind::Capture(name) => {
                // Binds to the previous child. A capture with no child
                // before it is an orphan (the root capture lives AFTER
                // the closing paren).
                let target = node.children.last_mut().ok_or(QueryError {
                    span: tok.span.clone(),
                    message: "capture before any child".into(),
                })?;
                target.capture = Some(name.clone());
                *idx += 1;
            }
            TokKind::Quant(q) => {
                let target = if node.children.is_empty() {
                    &mut node
                } else {
                    node.children.last_mut().expect("checked")
                };
                target.quant = *q;
                *idx += 1;
            }
            other => {
                return Err(QueryError {
                    span: tok.span.clone(),
                    message: format!("unexpected {other:?} in node body"),
                });
            }
        }
    }
    if !closed {
        return Err(QueryError {
            span: node.span.clone(),
            message: "unterminated node".into(),
        });
    }
    let node_preds = std::mem::take(&mut node_preds);
    // Post-`)` postfix: a capture/quantifier here binds this node (the
    // root capture in `(...) @root`).
    loop {
        match toks.get(*idx).map(|t| &t.kind) {
            Some(TokKind::Capture(name)) => {
                node.capture = Some(name.clone());
                *idx += 1;
            }
            Some(TokKind::Quant(q)) => {
                node.quant = *q;
                *idx += 1;
            }
            _ => break,
        }
    }
    Ok((node, node_preds))
}

/// `(#eq? @cap "value")` starting at the `(`.
fn parse_predicate(toks: &[Tok], idx: &mut usize) -> Result<PredicateDef, QueryError> {
    let pred_start = toks[*idx].span.start;
    *idx += 1; // (
    let Some(TokKind::Hash(pname)) = toks.get(*idx).map(|t| t.kind.clone()) else {
        return Err(QueryError {
            span: pred_start
                ..toks
                    .get(idx.saturating_sub(1))
                    .map_or(pred_start, |t| t.span.end),
            message: "expected a predicate name".into(),
        });
    };
    *idx += 1;
    if pname != "eq?" {
        return Err(QueryError {
            span: pred_start
                ..toks
                    .get(idx.saturating_sub(1))
                    .map_or(pred_start, |t| t.span.end),
            message: format!("unsupported predicate `#{pname}` (only `#eq?`)"),
        });
    }
    let Some(TokKind::Capture(cap)) = toks.get(*idx).map(|t| t.kind.clone()) else {
        return Err(QueryError {
            span: pred_start
                ..toks
                    .get(idx.saturating_sub(1))
                    .map_or(pred_start, |t| t.span.end),
            message: "expected @capture".into(),
        });
    };
    *idx += 1;
    let Some(TokKind::Str(value)) = toks.get(*idx).map(|t| t.kind.clone()) else {
        return Err(QueryError {
            span: pred_start
                ..toks
                    .get(idx.saturating_sub(1))
                    .map_or(pred_start, |t| t.span.end),
            message: "expected a string literal".into(),
        });
    };
    *idx += 1;
    let Some(TokKind::RParen) = toks.get(*idx).map(|t| &t.kind) else {
        return Err(QueryError {
            span: pred_start
                ..toks
                    .get(idx.saturating_sub(1))
                    .map_or(pred_start, |t| t.span.end),
            message: "unterminated predicate".into(),
        });
    };
    let pred_end = toks[*idx].span.end;
    *idx += 1;
    Ok(PredicateDef {
        capture: cap,
        value,
        span: pred_start..pred_end,
    })
}
