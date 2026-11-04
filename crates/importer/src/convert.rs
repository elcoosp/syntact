//! grammar.js value tree → [`GirDoc`] conversion with the mapped /
//! dropped / why report (issue 037).
//!
//! The subset is deliberately narrow — `choice`, `seq`, `optional`,
//! `repeat`, `repeat1`, `field`, `alias`, `token`, `prec.left/right`,
//! regex and string literals, `extras`, `externals`. Everything else is
//! dropped **with a reason** and never dangles: rules that fail to
//! convert are omitted, so the emitted KDL always loads.

use std::collections::{BTreeMap, BTreeSet};

use gir_ir::{Associativity, PrattHint};
use gir_syntax::{
    ByteSpan, GirDoc, ItemSurf, LanguageMeta, Marker, RuleDecl, RuleKindSurf, SepCfg, SeqEl,
    TokenDecl, TokenKindSurf,
};

use crate::parser::{Module, Spanned, Val};
use crate::{Action, ImportReport, ReportEntry};

/// The expression mini-IR the converter evaluates into.
#[derive(Debug, Clone, PartialEq, Eq)]
enum E {
    /// `$ . name`
    Ref(String),
    /// `'literal'`
    Lit(String),
    /// `/regex/`
    Re(String),
    /// `choice(...)`
    Alt(Vec<E>),
    /// `seq(...)`
    Seq(Vec<E>),
    /// `optional(...)`
    Opt(Box<E>),
    /// `repeat(...)`
    Rep0(Box<E>),
    /// `repeat1(...)`
    Rep1(Box<E>),
    /// `token(...)`
    Tok(Box<E>),
}

/// Per-rule evaluation state threaded through the expression walk.
struct RuleEval {
    /// The rule being converted (report scope).
    name: String,
    /// `prec.left/right` target captured during the walk.
    pratt: Option<PrattHint>,
    /// The body went through `token(...)` (affects the report wording).
    tok_wrapped: bool,
}

/// Conversion context shared across the whole module.
struct Ctx {
    report: ImportReport,
    doc: GirDoc,
    /// Every declared rule/token name (synth collision domain).
    known: BTreeSet<String>,
    /// Per-root-rule synth counters (`{root}-{base}{n}`).
    counters: BTreeMap<String, u32>,
    /// Global counter for extras tokens.
    extra_counter: u32,
    /// Synthesized rules/tokens awaiting their parent rule (kept so a
    /// parent always precedes its helpers in declaration order).
    pending_rules: Vec<RuleDecl>,
    pending_tokens: Vec<TokenDecl>,
}

/// Convert a parsed module into the surface document + report.
/// Conversion reports instead of failing: dropped constructs never
/// abort the import.
pub(super) fn convert(module: &Module) -> (GirDoc, ImportReport) {
    let mut ctx = Ctx {
        report: ImportReport::default(),
        doc: GirDoc {
            language: LanguageMeta {
                name: String::new(),
                version: String::new(),
                extensions: Vec::new(),
                unicode: false,
                span: ByteSpan::empty(0),
            },
            tokens: Vec::new(),
            rules: Vec::new(),
        },
        known: BTreeSet::new(),
        counters: BTreeMap::new(),
        extra_counter: 0,
        pending_rules: Vec::new(),
        pending_tokens: Vec::new(),
    };

    report_skipped_statements(&mut ctx, module);
    map_grammar_metadata(&mut ctx, module);

    // Rule bodies first (extras may reference tokens created here).
    if let Some((_, rules_val)) = module.props.iter().find(|(k, _)| k == "rules") {
        if let Val::Object(rules) = &rules_val.val {
            for (name, body) in rules {
                convert_rule(&mut ctx, name, body);
            }
        }
    }

    report_grammar_constructs(&mut ctx, module);

    (ctx.doc, ctx.report)
}

/// Note top-level statements outside `module.exports = grammar(...)`.
fn report_skipped_statements(ctx: &mut Ctx, module: &Module) {
    for stmt in &module.skipped {
        let what = if stmt.is_empty() {
            "statement"
        } else {
            stmt.as_str()
        };
        ctx.report.push(ReportEntry {
            construct: "top-level",
            scope: String::new(),
            action: Action::Note,
            detail: format!("top-level statement `{what}` skipped (outside grammar)"),
        });
    }
}

/// Map `name` and flag unrepresentable grammar-level properties.
fn map_grammar_metadata(ctx: &mut Ctx, module: &Module) {
    for (key, value) in &module.props {
        match key.as_str() {
            "name" => {
                if let Val::Str(name) = &value.val {
                    ctx.doc.language.name.clone_from(name);
                }
            }
            "rules" | "extras" | "externals" | "word" | "supertypes" | "inline" | "conflicts" => {}
            other => ctx.report.push(ReportEntry {
                construct: "grammar-property",
                scope: other.to_owned(),
                action: Action::Dropped,
                detail: format!("grammar property `{other}` is not representable in gir"),
            }),
        }
    }
}

/// Convert `extras`/`externals`, drop `word`/`supertypes`/`inline`/
/// `conflicts` — in a fixed order for a deterministic report.
fn report_grammar_constructs(ctx: &mut Ctx, module: &Module) {
    for (key, value) in &module.props {
        match key.as_str() {
            "extras" => convert_extras(ctx, value),
            "externals" => convert_externals(ctx, value),
            "word" | "supertypes" | "inline" | "conflicts" => ctx.report.push(ReportEntry {
                construct: match key.as_str() {
                    "word" => "word",
                    "supertypes" => "supertypes",
                    "inline" => "inline",
                    _ => "conflicts",
                },
                scope: ctx.doc.language.name.clone(),
                action: Action::Dropped,
                detail: word_drop_detail(key),
            }),
            _ => {}
        }
    }
}

/// Why a grammar-level property was dropped.
fn word_drop_detail(key: &str) -> String {
    if key == "word" {
        "word rule (keyword extraction) is not representable".to_owned()
    } else {
        format!("`{key}` has no gir equivalent; subsumed by the canonical IR")
    }
}

/// Regex-escape a literal fragment for the merged `token(seq)` form.
fn regex_escape_literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(
            ch,
            '.' | '+' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '^' | '$' | '\\'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn convert_rule(ctx: &mut Ctx, name: &str, body: &Spanned) {
    ctx.known.insert(name.to_owned());
    let mut eval = RuleEval {
        name: name.to_owned(),
        pratt: None,
        tok_wrapped: false,
    };
    match eval_expr(ctx, &mut eval, body)
        .map_err(|reason| format!("rule `{name}` dropped: {reason}"))
        .and_then(|expr| lower_body(ctx, name, &expr, eval.pratt, name, true))
    {
        Ok(Lowered::Token(token)) => {
            ctx.known.insert(token.name.clone());
            let detail = if eval.tok_wrapped {
                format!("token(/…/) → token {}", token.name)
            } else {
                match token.kind {
                    TokenKindSurf::Re { .. } => format!("regex rule → token {}", token.name),
                    TokenKindSurf::Lit { .. } => format!("literal rule → token {}", token.name),
                }
            };
            ctx.doc.tokens.push(token);
            drain_pending(ctx);
            ctx.report.push(ReportEntry {
                construct: "rule",
                scope: name.to_owned(),
                action: Action::Mapped,
                detail,
            });
        }
        Ok(Lowered::Rule(rule)) => {
            ctx.known.insert(rule.name.clone());
            let detail = match &rule.kind {
                RuleKindSurf::Alt { refs } => {
                    format!("choice → alt ({} alternatives)", refs.len())
                }
                RuleKindSurf::Sep(_) => {
                    "repeat(seq(x, sep)) → sep sugar (allow-trailing)".to_owned()
                }
                RuleKindSurf::Seq { items } => format!("seq → seq ({} items)", items.len()),
            };
            ctx.doc.rules.push(rule);
            drain_pending(ctx);
            ctx.report.push(ReportEntry {
                construct: "rule",
                scope: name.to_owned(),
                action: Action::Mapped,
                detail,
            });
        }
        Err(reason) => ctx.report.push(ReportEntry {
            construct: "rule",
            scope: name.to_owned(),
            action: Action::Dropped,
            detail: reason,
        }),
    }
}

/// Move synthesized helpers into the document behind their parent.
fn drain_pending(ctx: &mut Ctx) {
    ctx.doc.rules.append(&mut ctx.pending_rules);
    ctx.doc.tokens.append(&mut ctx.pending_tokens);
}

/// Evaluate a JS value tree into the expression mini-IR.
fn eval_expr(ctx: &mut Ctx, eval: &mut RuleEval, val: &Spanned) -> Result<E, String> {
    match &val.val {
        Val::Member { obj, prop } => {
            if let Val::Ident(handle) = &obj.val {
                if handle == "$" {
                    return Ok(E::Ref(prop.clone()));
                }
            }
            Err("unsupported member expression (only `$.rule` is in the subset)".to_owned())
        }
        Val::Str(text) => Ok(E::Lit(text.clone())),
        Val::Regex(body, flags) => {
            if !flags.is_empty() {
                ctx.report.push(ReportEntry {
                    construct: "regex-flags",
                    scope: eval.name.clone(),
                    action: Action::Note,
                    detail: format!("regex flags `{flags}` dropped (gir regexes are flagless)"),
                });
            }
            Ok(E::Re(body.clone()))
        }
        Val::Arrow { body } => eval_expr(ctx, eval, body),
        Val::Call { callee, args } => eval_call(ctx, eval, callee, args),
        Val::Ident(other) => Err(format!(
            "bare identifier `{other}` (helper functions are outside the subset)"
        )),
        Val::Num(text) => Err(format!("bare number `{text}` in a rule body")),
        Val::Array(_) | Val::Object(_) => {
            Err("array/object literal in a rule body (outside the subset)".to_owned())
        }
    }
}

fn eval_call(
    ctx: &mut Ctx,
    eval: &mut RuleEval,
    callee: &str,
    args: &[Spanned],
) -> Result<E, String> {
    let first_arg = || {
        args.first()
            .ok_or_else(|| format!("`{callee}` needs an argument"))
    };
    match callee {
        "choice" => {
            let mut items = Vec::with_capacity(args.len());
            for a in args {
                items.push(eval_expr(ctx, eval, a)?);
            }
            Ok(E::Alt(items))
        }
        "seq" => {
            let mut items = Vec::with_capacity(args.len());
            for a in args {
                items.push(eval_expr(ctx, eval, a)?);
            }
            Ok(E::Seq(items))
        }
        "optional" => Ok(E::Opt(Box::new(eval_expr(ctx, eval, first_arg()?)?))),
        "repeat" => Ok(E::Rep0(Box::new(eval_expr(ctx, eval, first_arg()?)?))),
        "repeat1" => Ok(E::Rep1(Box::new(eval_expr(ctx, eval, first_arg()?)?))),
        "token" => {
            eval.tok_wrapped = true;
            Ok(E::Tok(Box::new(eval_expr(ctx, eval, first_arg()?)?)))
        }
        "token.immediate" => {
            eval.tok_wrapped = true;
            ctx.report.push(ReportEntry {
                construct: "token.immediate",
                scope: eval.name.clone(),
                action: Action::Note,
                detail: "token.immediate → token (gir lexing is trivia-free by construction)"
                    .to_owned(),
            });
            Ok(E::Tok(Box::new(eval_expr(ctx, eval, first_arg()?)?)))
        }
        "field" => {
            let [field_arg, inner] = args else {
                return Err("`field(name, value)` takes two arguments".to_owned());
            };
            let Val::Str(field_name) = &field_arg.val else {
                return Err("`field` name must be a string".to_owned());
            };
            ctx.report.push(ReportEntry {
                construct: "field",
                scope: eval.name.clone(),
                action: Action::Note,
                detail: format!(
                    "field '{field_name}' dropped (gir has no fields); child kept bare"
                ),
            });
            eval_expr(ctx, eval, inner)
        }
        "alias" => {
            let inner = first_arg()?;
            ctx.report.push(ReportEntry {
                construct: "alias",
                scope: eval.name.clone(),
                action: Action::Note,
                detail: "alias dropped (gir has no aliasing); inner expression kept".to_owned(),
            });
            eval_expr(ctx, eval, inner)
        }
        "prec.left" | "prec.right" => eval_directed_prec(ctx, eval, callee, args),
        "prec" | "prec.dynamic" => eval_plain_prec(ctx, eval, callee, args),
        other => Err(format!(
            "unsupported call `{other}(...)` (outside the grammar.js subset)"
        )),
    }
}

/// Split a `prec*` argument list into its (optional) numeric label and
/// the wrapped body.
fn split_prec_args<'a>(callee: &str, args: &'a [Spanned]) -> Result<(String, &'a Spanned), String> {
    match args {
        [num, body] if matches!(num.val, Val::Num(_)) => {
            let Val::Num(text) = &num.val else {
                unreachable!("guarded by the matches! above")
            };
            Ok((text.clone(), body))
        }
        [body] => Ok(("0".to_owned(), body)),
        _ => Err(format!(
            "`{callee}` takes (number, expression) or (expression)"
        )),
    }
}

/// `prec.left(N, body)` / `prec.right(N, body)`: a real binding power
/// becomes the rule's pratt hint; everything else is reported.
fn eval_directed_prec(
    ctx: &mut Ctx,
    eval: &mut RuleEval,
    callee: &str,
    args: &[Spanned],
) -> Result<E, String> {
    let (bp, body) = split_prec_args(callee, args)?;
    let parsed: u32 = bp.parse().map_err(|_| format!("bad precedence `{bp}`"))?;
    let assoc = if callee == "prec.left" {
        Associativity::Left
    } else {
        Associativity::Right
    };
    let word = if assoc == Associativity::Left {
        "left"
    } else {
        "right"
    };
    if parsed == 0 {
        ctx.report.push(ReportEntry {
            construct: "prec",
            scope: eval.name.clone(),
            action: Action::Note,
            detail: format!(
                "{callee} without a precedence number: hint dropped (gir needs lbp ≥ 1)"
            ),
        });
    } else if parsed > u8::MAX.into() {
        ctx.report.push(ReportEntry {
            construct: "prec",
            scope: eval.name.clone(),
            action: Action::Note,
            detail: format!("precedence {parsed} does not fit u8; hint dropped"),
        });
    } else {
        ctx.report.push(ReportEntry {
            construct: "prec",
            scope: eval.name.clone(),
            action: Action::Mapped,
            detail: format!("{callee}({parsed}) → pratt hint (assoc {word}, bp {parsed})"),
        });
        eval.pratt = Some(PrattHint {
            left_bp: u8::try_from(parsed).unwrap_or(u8::MAX),
            right_bp: u8::try_from(parsed).unwrap_or(u8::MAX),
            assoc,
        });
    }
    eval_expr(ctx, eval, body)
}

/// `prec(N, body)` / `prec.dynamic(N, body)`: priority hints have no
/// gir equivalent; the wrapper is dropped with a note.
fn eval_plain_prec(
    ctx: &mut Ctx,
    eval: &mut RuleEval,
    callee: &str,
    args: &[Spanned],
) -> Result<E, String> {
    let (label, body) = split_prec_args(callee, args)?;
    ctx.report.push(ReportEntry {
        construct: "prec",
        scope: eval.name.clone(),
        action: Action::Note,
        detail: format!(
            "{callee}({label}) is a priority hint with no gir equivalent; wrapper dropped"
        ),
    });
    eval_expr(ctx, eval, body)
}

/// What a rule body lowers to.
enum Lowered {
    Token(TokenDecl),
    Rule(RuleDecl),
}

/// Allocate a deterministic synthesized name: `{root}-{base}{n}`.
fn synth_name(ctx: &mut Ctx, root: &str, base: &str) -> String {
    let counter = ctx.counters.entry(root.to_owned()).or_default();
    loop {
        *counter += 1;
        let candidate = format!("{root}-{base}{counter}");
        if !ctx.known.contains(&candidate) {
            return candidate;
        }
    }
}

/// Is this body the separated-list pattern `repeat(seq(x, "sep"))`?
fn sep_pattern(expr: &E) -> Option<(&str, &str)> {
    match expr {
        E::Rep0(inner) => match inner.as_ref() {
            E::Seq(items) => match items.as_slice() {
                [E::Ref(name), E::Lit(sep)] => Some((name, sep)),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

fn token_decl(name: String, kind: TokenKindSurf, node: bool) -> TokenDecl {
    TokenDecl {
        name,
        kind,
        node,
        trivia: false,
        index_bound: None,
        span: ByteSpan::empty(0),
        name_span: ByteSpan::empty(0),
    }
}

fn rule_decl(name: String, kind: RuleKindSurf, node: bool, pratt: Option<PrattHint>) -> RuleDecl {
    RuleDecl {
        name,
        kind,
        node,
        reuse: false,
        wrap_error: false,
        pratt,
        span: ByteSpan::empty(0),
        name_span: ByteSpan::empty(0),
    }
}

fn lower_body(
    ctx: &mut Ctx,
    name: &str,
    expr: &E,
    pratt: Option<PrattHint>,
    root: &str,
    node: bool,
) -> Result<Lowered, String> {
    match expr {
        E::Re(body) => Ok(Lowered::Token(token_decl(
            name.to_owned(),
            TokenKindSurf::Re {
                regex: body.clone(),
            },
            true,
        ))),
        E::Lit(text) => Ok(Lowered::Token(token_decl(
            name.to_owned(),
            TokenKindSurf::Lit { text: text.clone() },
            true,
        ))),
        E::Tok(inner) => lower_token_wrap(ctx, name, inner),
        E::Ref(target) => Ok(Lowered::Rule(rule_decl(
            name.to_owned(),
            RuleKindSurf::Alt {
                refs: vec![(target.clone(), ByteSpan::empty(0))],
            },
            node,
            pratt,
        ))),
        E::Alt(items) => {
            let refs = items
                .iter()
                .map(|item| simple_ref(ctx, root, item))
                .collect::<Vec<_>>();
            Ok(Lowered::Rule(rule_decl(
                name.to_owned(),
                RuleKindSurf::Alt {
                    refs: refs.into_iter().map(|r| (r, ByteSpan::empty(0))).collect(),
                },
                node,
                pratt,
            )))
        }
        other => Ok(lower_as_seq_rule(ctx, name, other, pratt, root, node)),
    }
}

/// Lower a body that is not a bare token/alt: the whole-body sep sugar,
/// a `kind=seq` with markers, or one-item marker sequences.
fn lower_as_seq_rule(
    ctx: &mut Ctx,
    name: &str,
    expr: &E,
    pratt: Option<PrattHint>,
    root: &str,
    node: bool,
) -> Lowered {
    if let Some((element, separator)) = sep_pattern(expr) {
        return Lowered::Rule(rule_decl(
            name.to_owned(),
            RuleKindSurf::Sep(SepCfg {
                element: element.to_owned(),
                element_span: ByteSpan::empty(0),
                separator: separator.to_owned(),
                allow_trailing: true,
            }),
            node,
            pratt,
        ));
    }
    let items = match expr {
        E::Seq(items) => items.iter().map(|i| lower_item(ctx, root, i)).collect(),
        E::Opt(inner) => vec![with_marker(Marker::Opt, lower_item(ctx, root, inner))],
        E::Rep0(inner) => vec![with_marker(Marker::Rep, lower_item(ctx, root, inner))],
        E::Rep1(inner) => vec![with_marker(Marker::Rep1, lower_item(ctx, root, inner))],
        // Anything else (nested Tok of a complex body) cannot appear
        // here: `lower_body` routes those to `lower_token_wrap`.
        _ => Vec::new(),
    };
    Lowered::Rule(rule_decl(
        name.to_owned(),
        RuleKindSurf::Seq { items },
        node,
        pratt,
    ))
}

fn with_marker(marker: Marker, mut el: SeqEl) -> SeqEl {
    el.marker = Some(marker);
    el
}

/// One seq item: literals/refs stay; regexes become anonymous tokens;
/// everything complex becomes a synthesized sub-rule. Markers bind
/// backward to the emitted item.
fn lower_item(ctx: &mut Ctx, root: &str, expr: &E) -> SeqEl {
    // Sep-shaped children become sep sub-rules first — before any
    // marker unwrapping, since the sugar subsumes the repetition.
    if let Some((element, separator)) = sep_pattern(expr) {
        let child = synth_name(ctx, root, "list");
        ctx.known.insert(child.clone());
        ctx.pending_rules.push(rule_decl(
            child.clone(),
            RuleKindSurf::Sep(SepCfg {
                element: element.to_owned(),
                element_span: ByteSpan::empty(0),
                separator: separator.to_owned(),
                allow_trailing: true,
            }),
            false,
            None,
        ));
        ctx.report.push(ReportEntry {
            construct: "sep",
            scope: child.clone(),
            action: Action::Mapped,
            detail: "repeat(seq(x, sep)) → sep sugar (allow-trailing)".to_owned(),
        });
        return SeqEl {
            item: ItemSurf::Ref { name: child },
            marker: None,
            span: ByteSpan::empty(0),
        };
    }
    let (base_expr, marker) = match expr {
        E::Opt(inner) => (inner.as_ref(), Some(Marker::Opt)),
        E::Rep0(inner) => (inner.as_ref(), Some(Marker::Rep)),
        E::Rep1(inner) => (inner.as_ref(), Some(Marker::Rep1)),
        other => (other, None),
    };
    let item = match base_expr {
        E::Ref(name) => ItemSurf::Ref { name: name.clone() },
        E::Lit(text) => ItemSurf::Lit { text: text.clone() },
        E::Re(body) => {
            let tok = synth_name(ctx, root, "re");
            ctx.known.insert(tok.clone());
            ctx.pending_tokens.push(token_decl(
                tok.clone(),
                TokenKindSurf::Re {
                    regex: body.clone(),
                },
                false,
            ));
            ItemSurf::Ref { name: tok }
        }
        other => {
            let child = synth_name(ctx, root, "item");
            ctx.known.insert(child.clone());
            // Children synthesized during the body's lowering append to
            // the pending buffer; inserting the parent at the recorded
            // index keeps parents before their helpers.
            let at = ctx.pending_rules.len() + ctx.pending_tokens.len();
            let lowered = lower_body(ctx, &child, other, None, root, false);
            match lowered {
                Ok(Lowered::Rule(rule)) => insert_pending_rule(ctx, at, rule),
                Ok(Lowered::Token(tok)) => insert_pending_token(ctx, at, tok),
                Err(reason) => ctx.report.push(crate::ReportEntry {
                    construct: "rule",
                    scope: child.clone(),
                    action: crate::Action::Dropped,
                    detail: reason,
                }),
            }
            ItemSurf::Ref { name: child }
        }
    };
    SeqEl {
        item,
        marker,
        span: ByteSpan::empty(0),
    }
}

/// Insert a synthesized rule so it precedes helpers made by its body.
fn insert_pending_rule(ctx: &mut Ctx, at: usize, rule: RuleDecl) {
    let index = at.min(ctx.pending_rules.len());
    ctx.pending_rules.insert(index, rule);
}

/// Token twin of [`insert_pending_rule`]: the split index accounts for
/// tokens synthesized alongside rules.
fn insert_pending_token(ctx: &mut Ctx, at: usize, tok: TokenDecl) {
    let index = at.min(ctx.pending_tokens.len());
    ctx.pending_tokens.insert(index, tok);
}

/// Resolve an alt member to a reference name, synthesizing anonymous
/// tokens for literals/regexes and sub-rules for complex members.
fn simple_ref(ctx: &mut Ctx, root: &str, expr: &E) -> String {
    match expr {
        E::Ref(name) => name.clone(),
        E::Lit(text) => {
            let tok = synth_name(ctx, root, "lit");
            ctx.known.insert(tok.clone());
            ctx.doc.tokens.push(token_decl(
                tok.clone(),
                TokenKindSurf::Lit { text: text.clone() },
                false,
            ));
            tok
        }
        E::Re(body) => {
            let tok = synth_name(ctx, root, "re");
            ctx.known.insert(tok.clone());
            ctx.doc.tokens.push(token_decl(
                tok.clone(),
                TokenKindSurf::Re {
                    regex: body.clone(),
                },
                false,
            ));
            tok
        }
        other => {
            let child = synth_name(ctx, root, "item");
            ctx.known.insert(child.clone());
            let at = ctx.pending_rules.len() + ctx.pending_tokens.len();
            match lower_body(ctx, &child, other, None, root, false) {
                Ok(Lowered::Rule(rule)) => insert_pending_rule(ctx, at, rule),
                Ok(Lowered::Token(tok)) => insert_pending_token(ctx, at, tok),
                Err(reason) => ctx.report.push(crate::ReportEntry {
                    construct: "rule",
                    scope: child.clone(),
                    action: crate::Action::Dropped,
                    detail: reason,
                }),
            }
            child
        }
    }
}

fn lower_token_wrap(ctx: &mut Ctx, name: &str, inner: &E) -> Result<Lowered, String> {
    match inner {
        E::Lit(text) => Ok(Lowered::Token(token_decl(
            name.to_owned(),
            TokenKindSurf::Lit { text: text.clone() },
            true,
        ))),
        E::Re(body) => Ok(Lowered::Token(token_decl(
            name.to_owned(),
            TokenKindSurf::Re {
                regex: body.clone(),
            },
            true,
        ))),
        E::Seq(items) if items.iter().all(|i| matches!(i, E::Lit(_) | E::Re(_))) => {
            // Regex-decided literals: literal fragments are regex-escaped
            // and concatenated with the regex fragments into one pattern.
            let mut merged = String::new();
            for item in items {
                match item {
                    E::Lit(text) => merged.push_str(&regex_escape_literal(text)),
                    E::Re(body) => merged.push_str(body),
                    _ => unreachable!("guarded by the all() filter"),
                }
            }
            ctx.report.push(ReportEntry {
                construct: "token-merge",
                scope: name.to_owned(),
                action: Action::Mapped,
                detail: "token(seq(lit, regex)) merged to regex".to_owned(),
            });
            Ok(Lowered::Token(token_decl(
                name.to_owned(),
                TokenKindSurf::Re { regex: merged },
                true,
            )))
        }
        _ => Err("token() of a complex body is not representable".to_owned()),
    }
}

fn convert_extras(ctx: &mut Ctx, value: &Spanned) {
    let items: Vec<Spanned> = match &value.val {
        Val::Arrow { body } => match &body.val {
            Val::Array(items) => items.clone(),
            single => vec![Spanned {
                val: single.clone(),
                offset: body.offset,
            }],
        },
        Val::Array(items) => items.clone(),
        _ => {
            ctx.report.push(ReportEntry {
                construct: "extras",
                scope: ctx.doc.language.name.clone(),
                action: Action::Dropped,
                detail: "extras must be an array".to_owned(),
            });
            return;
        }
    };
    for item in items {
        match &item.val {
            Val::Regex(body, _) => {
                ctx.extra_counter += 1;
                let name = format!("extra{}", ctx.extra_counter);
                ctx.known.insert(name.clone());
                ctx.doc.tokens.push(TokenDecl {
                    name: name.clone(),
                    kind: TokenKindSurf::Re {
                        regex: body.clone(),
                    },
                    node: false,
                    trivia: true,
                    index_bound: None,
                    span: ByteSpan::empty(0),
                    name_span: ByteSpan::empty(0),
                });
                ctx.report.push(ReportEntry {
                    construct: "extras",
                    scope: name.clone(),
                    action: Action::Mapped,
                    detail: format!("extra /{body}/ → trivia token {name}"),
                });
            }
            Val::Member { obj, prop } if matches!(obj.val, Val::Ident(ref h) if h == "$") => {
                match ctx.doc.tokens.iter_mut().find(|tok| tok.name == *prop) {
                    Some(token) => {
                        token.trivia = true;
                        ctx.report.push(ReportEntry {
                            construct: "extras",
                            scope: prop.clone(),
                            action: Action::Mapped,
                            detail: format!("extra $.{prop} → trivia on token {prop}"),
                        });
                    }
                    None => ctx.report.push(ReportEntry {
                        construct: "extras",
                        scope: prop.clone(),
                        action: Action::Dropped,
                        detail: format!("extra $.{prop} names no token; trivia not applied"),
                    }),
                }
            }
            _ => ctx.report.push(ReportEntry {
                construct: "extras",
                scope: ctx.doc.language.name.clone(),
                action: Action::Dropped,
                detail: "extra is neither a regex nor a rule reference".to_owned(),
            }),
        }
    }
}

fn convert_externals(ctx: &mut Ctx, value: &Spanned) {
    let items: Vec<Spanned> = match &value.val {
        Val::Arrow { body } => match &body.val {
            Val::Array(items) => items.clone(),
            _ => Vec::new(),
        },
        Val::Array(items) => items.clone(),
        _ => Vec::new(),
    };
    for item in items {
        let prop = match &item.val {
            Val::Member { obj, prop } if matches!(obj.val, Val::Ident(ref h) if h == "$") => {
                prop.clone()
            }
            _ => {
                ctx.report.push(ReportEntry {
                    construct: "externals",
                    scope: ctx.doc.language.name.clone(),
                    action: Action::Dropped,
                    detail: "external is not a `$.name` reference".to_owned(),
                });
                continue;
            }
        };
        let mut name = prop.clone();
        if ctx.known.contains(&name) {
            name.push_str("-ext");
        }
        ctx.known.insert(name.clone());
        ctx.doc.tokens.push(TokenDecl {
            name: name.clone(),
            kind: TokenKindSurf::Lit {
                text: format!("<external:{prop}>"),
            },
            node: true,
            trivia: false,
            index_bound: None,
            span: ByteSpan::empty(0),
            name_span: ByteSpan::empty(0),
        });
        ctx.report.push(ReportEntry {
            construct: "externals",
            scope: name.clone(),
            action: Action::Mapped,
            detail: format!("external $.{prop} → placeholder token (manual wiring required)"),
        });
    }
}
