//! Surface → canonical IR lowering (issue 004): the separated-list sugar
//! and anonymous literals resolve into [`gir_ir`] elements.

use std::collections::HashMap;

use gir_ir::{Associativity, Element, Grammar, GrammarBuilder, TokenId};

use crate::doc::{GirDoc, ItemSurf, Marker, RuleKindSurf, TokenKindSurf};
use crate::error::{ByteSpan, SurfaceError};

impl GirDoc {
    /// Lower into the canonical typed IR.
    ///
    /// # Errors
    /// [`SurfaceError::UnknownRef`] for references to undeclared names,
    /// [`SurfaceError::Ir`] for IR validation failures (empty grammar,
    /// duplicate names that slipped past the loader, empty alternatives).
    pub fn to_grammar(&self) -> Result<Grammar, SurfaceError> {
        let mut builder = GrammarBuilder::new(&self.language.name);
        builder.version(&self.language.version);
        for extension in &self.language.extensions {
            builder.extension(extension);
        }
        if self.language.unicode {
            builder.unicode();
        }

        // Declared literal tokens double as the anonymous-literal dedup
        // table: `"{"` in a rule body reuses `lbrace` instead of minting
        // a duplicate token for the same text.
        let mut declared_lits: HashMap<String, TokenId> = HashMap::new();
        for token in &self.tokens {
            let id = match &token.kind {
                TokenKindSurf::Lit { text } => {
                    let id = builder.token_lit(&token.name, text);
                    if let Ok(id) = id {
                        declared_lits.insert(text.clone(), id);
                    }
                    id
                }
                TokenKindSurf::Re { regex } => builder.token_re(&token.name, regex),
            }
            .map_err(|err| SurfaceError::Ir {
                span: token.span,
                message: err.to_string(),
            })?;
            builder.set_node(id, token.node);
            builder.set_trivia(id, token.trivia);
            if let Some(bound) = &token.index_bound {
                builder.set_index_bound(id, bound);
            }
        }

        for rule in &self.rules {
            builder
                .declare_rule(&rule.name)
                .map_err(|err| SurfaceError::Ir {
                    span: rule.span,
                    message: err.to_string(),
                })?;
        }

        let mut literals = AnonLiterals {
            by_text: declared_lits,
            used_names: std::collections::HashSet::new(),
        };
        for rule in &self.rules {
            let id = builder
                .rule_id(&rule.name)
                .ok_or(SurfaceError::UnknownRef {
                    name: rule.name.clone(),
                    span: rule.name_span,
                })?;
            let body = Self::lower_kind(&rule.kind, &mut builder, &mut literals)?;
            builder.set_body(id, body);
            builder.set_node_rule(id, rule.node);
            builder.set_reuse(id, rule.reuse);
            builder.set_wrap_error(id, rule.wrap_error);
            if let Some(pratt) = rule.pratt {
                builder.set_pratt(id, pratt);
            }
            if matches!(rule.kind, RuleKindSurf::Sep(_)) {
                builder.set_list(id, true);
            }
        }

        builder.finish().map_err(|err| SurfaceError::Ir {
            span: ByteSpan::empty(0),
            message: err.to_string(),
        })
    }

    fn lower_kind(
        kind: &RuleKindSurf,
        builder: &mut GrammarBuilder,
        literals: &mut AnonLiterals,
    ) -> Result<Element, SurfaceError> {
        match kind {
            RuleKindSurf::Alt { refs } => {
                let mut branches = Vec::with_capacity(refs.len());
                for (name, span) in refs {
                    branches.push(resolve(name, *span, builder)?);
                }
                Ok(GrammarBuilder::alt(branches))
            }
            RuleKindSurf::Seq { items } => {
                let mut elements: Vec<Element> = Vec::with_capacity(items.len());
                for item in items {
                    let element = match &item.item {
                        ItemSurf::Ref { name } => resolve(name, item.span, builder)?,
                        ItemSurf::Lit { text } => Element::Token(literals.literal(text, builder)?),
                    };
                    match item.marker {
                        None => elements.push(element),
                        Some(marker) => {
                            // Markers bind backward: the loader attached
                            // the marker word to this very item, so the
                            // item's own element gets wrapped.
                            elements.push(wrap(marker, element));
                        }
                    }
                }
                Ok(GrammarBuilder::seq(elements))
            }
            RuleKindSurf::Sep(cfg) => {
                let element = resolve(&cfg.element, cfg.element_span, builder)?;
                let separator = Element::Token(literals.literal(&cfg.separator, builder)?);
                // element (separator element)* [separator] — the
                // separated-list sugar (issue 004).
                let mut parts = vec![
                    element.clone(),
                    GrammarBuilder::rep0(GrammarBuilder::seq(vec![separator.clone(), element])),
                ];
                if cfg.allow_trailing {
                    parts.push(GrammarBuilder::opt(GrammarBuilder::seq(vec![separator])));
                }
                Ok(GrammarBuilder::seq(parts))
            }
        }
    }
}

/// Resolve a surface reference: rules first, then tokens.
fn resolve(name: &str, span: ByteSpan, builder: &GrammarBuilder) -> Result<Element, SurfaceError> {
    if let Some(id) = builder.rule_id(name) {
        return Ok(Element::Rule(id));
    }
    if let Some(id) = builder.token_id(name) {
        return Ok(Element::Token(id));
    }
    Err(SurfaceError::UnknownRef {
        name: name.to_owned(),
        span,
    })
}

fn wrap(marker: Marker, element: Element) -> Element {
    match marker {
        Marker::Opt => GrammarBuilder::opt(element),
        Marker::Rep => GrammarBuilder::rep0(element),
        Marker::Rep1 => GrammarBuilder::rep1(element),
        Marker::And => GrammarBuilder::and(element),
        Marker::Not => GrammarBuilder::not(element),
    }
}

/// Anonymous literal tokens: `Text → TokenId`, deterministic names.
#[derive(Default)]
struct AnonLiterals {
    by_text: HashMap<String, TokenId>,
    used_names: std::collections::HashSet<String>,
}

impl AnonLiterals {
    fn literal(
        &mut self,
        text: &str,
        builder: &mut GrammarBuilder,
    ) -> Result<TokenId, SurfaceError> {
        if let Some(&id) = self.by_text.get(text) {
            return Ok(id);
        }
        let base = format!("lit_{}", escape_name(text));
        let mut name = base.clone();
        let mut suffix = 1u32;
        while self.used_names.contains(&name) {
            suffix += 1;
            name = format!("{base}_{suffix}");
        }
        self.used_names.insert(name.clone());
        let id = builder
            .token_lit(&name, text)
            .map_err(|err| SurfaceError::Ir {
                span: ByteSpan::empty(0),
                message: err.to_string(),
            })?;
        self.by_text.insert(text.to_owned(), id);
        Ok(id)
    }
}

fn escape_name(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
        } else if ch == '-' {
            out.push_str("dash");
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push('_');
    }
    out
}

/// Convenience: load KDL text and lower straight to IR.
///
/// # Errors
/// Anything [`load`] or [`GirDoc::to_grammar`] reports.
pub fn load_grammar(text: &str) -> Result<Grammar, SurfaceError> {
    crate::load(text)?.to_grammar()
}

/// Which associativity a rule carries (used by render/tests).
#[must_use]
pub fn assoc_word(assoc: Associativity) -> &'static str {
    match assoc {
        Associativity::Left => "left",
        Associativity::Right => "right",
        Associativity::None => "none",
    }
}
