//! Canonical KDL rendering: [`GirDoc`] → text, round-trippable through
//! [`load`](crate::load) (issue 004 round-trip requirement).

use crate::doc::{GirDoc, ItemSurf, RuleKindSurf, TokenKindSurf};
use crate::lower::assoc_word;

/// Render a surface document to canonical KDL text (stable formatting:
/// two-space indent, properties in fixed order).
#[must_use]
pub fn render(doc: &GirDoc) -> String {
    let mut out = String::new();
    render_language(doc, &mut out);
    if !doc.tokens.is_empty() {
        out.push_str("\ntokens {\n");
        for token in &doc.tokens {
            render_token(token, &mut out);
        }
        out.push_str("}\n");
    }
    if !doc.rules.is_empty() {
        out.push_str("\ngrammar {\n");
        for rule in &doc.rules {
            render_rule(rule, &mut out);
        }
        out.push_str("}\n");
    }
    out
}

fn render_language(doc: &GirDoc, out: &mut String) {
    out.push_str("language ");
    out.push_str(&quote(&doc.language.name));
    if !doc.language.version.is_empty() {
        out.push_str(" version=");
        out.push_str(&quote(&doc.language.version));
    }
    for extension in &doc.language.extensions {
        out.push_str(" extension=");
        out.push_str(&quote(extension));
    }
    if doc.language.unicode {
        out.push_str(" unicode=\"true\"");
    }
    out.push('\n');
}

fn render_token(token: &crate::doc::TokenDecl, out: &mut String) {
    out.push_str("    ");
    out.push_str(&token.name);
    match &token.kind {
        TokenKindSurf::Lit { text } => {
            out.push_str(" kind=lit ");
            out.push_str(&quote(text));
        }
        TokenKindSurf::Re { regex } => {
            out.push_str(" kind=re ");
            out.push_str(&quote(regex));
        }
    }
    if token.node {
        out.push_str(" node=\"true\"");
    }
    if token.trivia {
        out.push_str(" trivia=\"true\"");
    }
    if let Some(bound) = &token.index_bound {
        out.push_str(" index-bound=");
        out.push_str(&quote(bound));
    }
    out.push('\n');
}

fn render_rule(rule: &crate::doc::RuleDecl, out: &mut String) {
    out.push_str("    ");
    out.push_str(&rule.name);
    match &rule.kind {
        RuleKindSurf::Alt { refs } => {
            out.push_str(" kind=alt");
            for (name, _) in refs {
                out.push(' ');
                out.push_str(name);
            }
        }
        RuleKindSurf::Seq { items } => {
            out.push_str(" kind=seq");
            for item in items {
                out.push(' ');
                match &item.item {
                    ItemSurf::Ref { name } => out.push_str(name),
                    ItemSurf::Lit { text } => out.push_str(&quote(text)),
                }
                if let Some(marker) = item.marker {
                    out.push(' ');
                    out.push_str(marker.word());
                }
            }
        }
        RuleKindSurf::Sep(cfg) => {
            out.push_str(" kind=sep element=");
            out.push_str(&cfg.element);
            out.push_str(" separator=");
            out.push_str(&quote(&cfg.separator));
            if cfg.allow_trailing {
                out.push_str(" allow-trailing=\"true\"");
            }
        }
    }
    if rule.node {
        out.push_str(" node=\"true\"");
    }
    if rule.reuse {
        out.push_str(" reuse=\"true\"");
    }
    if rule.wrap_error {
        out.push_str(" wrap-error=\"true\"");
    }
    if let Some(pratt) = rule.pratt {
        out.push_str(" lbp=");
        out.push_str(&pratt.left_bp.to_string());
        out.push_str(" rbp=");
        out.push_str(&pratt.right_bp.to_string());
        out.push_str(" assoc=");
        out.push_str(assoc_word(pratt.assoc));
    }
    out.push('\n');
}

/// KDL string quoting: wraps in `"` and escapes `\`, `"`, and control
/// characters.
fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
