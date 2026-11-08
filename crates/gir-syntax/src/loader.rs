//! KDL loading: `.gir` text → span-preserving [`GirDoc`] (issue 004).
//!
//! The surface grammar (valid KDL throughout):
//!
//! ```kdl
//! language "json" version="1.0.0" extension="json" unicode="true"
//!
//! tokens {
//!     number kind=re "[0-9]+" node="true"
//!     lbrace kind=lit "{"
//! }
//!
//! grammar {
//!     value     kind=alt object array string number
//!     object    kind=seq "{" pair-list opt "}" node="true" reuse="true"
//!     pair-list kind=sep element=pair separator="," allow-trailing="true"
//!     pair      kind=seq string ":" value
//! }
//! ```
//!
//! `(seq)` markers (`opt`, `rep`, `rep1`, `and`, `not`) are bare
//! identifiers that bind **backward** to the preceding element. Quoted
//! strings inside a `(seq)` body are anonymous literal tokens.

use kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};

use crate::doc::{
    GirDoc, ItemSurf, LanguageMeta, Marker, RuleDecl, RuleKindSurf, SepCfg, SeqEl, TokenDecl,
    TokenKindSurf,
};
use crate::error::{ByteSpan, SurfaceError};

/// Reserved words usable as backward-binding markers in `(seq)` bodies.
pub const RESERVED_MARKERS: [&str; 5] = ["opt", "rep", "rep1", "and", "not"];

/// Parse `.gir` KDL text into a span-preserving [`GirDoc`].
///
/// # Errors
/// [`SurfaceError`] with the offending [`ByteSpan`] for any structural
/// problem: unknown sections, unknown kinds, duplicate names, malformed
/// entries, reserved words.
pub fn load(text: &str) -> Result<GirDoc, SurfaceError> {
    let doc = KdlDocument::parse(text).map_err(|err| {
        let diagnostic = err.diagnostics.first();
        let span = diagnostic.map_or_else(|| ByteSpan::empty(0), |d| ByteSpan::from(d.span));
        let message = diagnostic
            .and_then(|d| d.message.clone())
            .unwrap_or_else(|| "parse error".to_owned());
        SurfaceError::Kdl { span, message }
    })?;
    Loader::new(text).load_document(&doc)
}

struct Loader<'t> {
    text: &'t str,
    names: std::collections::HashSet<String>,
}

impl<'t> Loader<'t> {
    fn new(text: &'t str) -> Self {
        Self {
            text,
            names: std::collections::HashSet::new(),
        }
    }

    fn load_document(&mut self, doc: &KdlDocument) -> Result<GirDoc, SurfaceError> {
        let mut language: Option<LanguageMeta> = None;
        let mut tokens: Vec<TokenDecl> = Vec::new();
        let mut rules: Vec<RuleDecl> = Vec::new();

        for node in doc.nodes() {
            match node.name().value() {
                "language" => {
                    if language.is_some() {
                        return Err(SurfaceError::DuplicateLanguage {
                            span: span_of(node),
                        });
                    }
                    language = Some(Self::language_meta(node)?);
                }
                "tokens" => {
                    if let Some(children) = node.children() {
                        for child in children.nodes() {
                            tokens.push(self.token_decl(child)?);
                        }
                    }
                }
                "grammar" => {
                    if let Some(children) = node.children() {
                        for child in children.nodes() {
                            rules.push(self.rule_decl(child)?);
                        }
                    }
                }
                other => {
                    return Err(SurfaceError::UnknownSection {
                        name: other.to_owned(),
                        span: span_of(node),
                    });
                }
            }
        }

        let language = language.ok_or(SurfaceError::MissingLanguage)?;
        Ok(GirDoc {
            language,
            tokens,
            rules,
        })
    }

    fn language_meta(node: &KdlNode) -> Result<LanguageMeta, SurfaceError> {
        let span = span_of(node);
        let name = node
            .entries()
            .iter()
            .find(|entry| entry.name().is_none())
            .ok_or(SurfaceError::MissingLanguageName { span })?;
        let KdlValue::String(name) = name.value() else {
            let name_span = ByteSpan::from(name.span());
            return Err(SurfaceError::BadValue {
                expected: "a language name string",
                span: name_span,
            });
        };
        let mut meta = LanguageMeta {
            name: name.clone(),
            version: String::new(),
            extensions: Vec::new(),
            unicode: false,
            span,
        };
        if let Some(version) = prop(node, "version") {
            as_string(version)?.clone_into(&mut meta.version);
        }
        for extension in prop_all(node, "extension") {
            meta.extensions.push(as_string(extension)?.to_owned());
        }
        if let Some(unicode) = prop(node, "unicode") {
            meta.unicode = as_bool(unicode)?;
        }
        Ok(meta)
    }

    fn check_name(&mut self, name: &str, span: ByteSpan) -> Result<(), SurfaceError> {
        if RESERVED_MARKERS.contains(&name) {
            return Err(SurfaceError::ReservedName {
                name: name.to_owned(),
                span,
            });
        }
        if !self.names.insert(name.to_owned()) {
            return Err(SurfaceError::DuplicateName {
                name: name.to_owned(),
                span,
            });
        }
        Ok(())
    }

    fn token_decl(&mut self, node: &KdlNode) -> Result<TokenDecl, SurfaceError> {
        let span = span_of(node);
        let name_span = ByteSpan::from(node.name().span());
        let name = node.name().value().to_owned();
        self.check_name(&name, name_span)?;

        let args: Vec<&KdlEntry> = node
            .entries()
            .iter()
            .filter(|entry| entry.name().is_none())
            .collect();
        let kind = if let Some(kind_entry) = prop(node, "kind") {
            match as_string(kind_entry)? {
                "lit" | "re" => {
                    if args.len() != 1 {
                        return Err(SurfaceError::MissingArg {
                            expected: "kind=lit|re with exactly one match value",
                            span,
                        });
                    }
                    let text = as_string(args[0])?;
                    if matches!(kind_entry.value(), KdlValue::String(s) if s == "lit") {
                        TokenKindSurf::Lit {
                            text: text.to_owned(),
                        }
                    } else {
                        TokenKindSurf::Re {
                            regex: text.to_owned(),
                        }
                    }
                }
                other => {
                    return Err(SurfaceError::UnknownKind {
                        kind: other.to_owned(),
                        span: ByteSpan::from(kind_entry.span()),
                    });
                }
            }
        } else {
            // Shorthand: one quoted string argument is a literal.
            let [arg] = args.as_slice() else {
                return Err(SurfaceError::MissingArg {
                    expected: "kind=lit|re plus its match value, or one literal string",
                    span,
                });
            };
            TokenKindSurf::Lit {
                text: as_string(arg)?.to_owned(),
            }
        };

        let mut decl = TokenDecl {
            name,
            kind,
            node: false,
            trivia: false,
            index_bound: None,
            span,
            name_span,
        };
        if let Some(node_flag) = prop(node, "node") {
            decl.node = as_bool(node_flag)?;
        }
        if let Some(trivia) = prop(node, "trivia") {
            decl.trivia = as_bool(trivia)?;
        }
        if let Some(bound) = prop(node, "index-bound") {
            decl.index_bound = Some(as_string(bound)?.to_owned());
        }
        Ok(decl)
    }

    fn rule_decl(&mut self, node: &KdlNode) -> Result<RuleDecl, SurfaceError> {
        let span = span_of(node);
        let name_span = ByteSpan::from(node.name().span());
        let name = node.name().value().to_owned();
        self.check_name(&name, name_span)?;

        let kind_entry = prop(node, "kind").ok_or(SurfaceError::MissingArg {
            expected: "kind=alt|seq|sep",
            span,
        })?;
        let kind = match as_string(kind_entry)? {
            "alt" => self.alt_body(node)?,
            "seq" => self.seq_body(node)?,
            "sep" => self.sep_body(node)?,
            other => {
                return Err(SurfaceError::UnknownKind {
                    kind: other.to_owned(),
                    span: ByteSpan::from(kind_entry.span()),
                });
            }
        };

        let mut decl = RuleDecl {
            name,
            kind,
            node: flag(node, "node")?,
            reuse: flag(node, "reuse")?,
            wrap_error: flag(node, "wrap-error")?,
            pratt: None,
            span,
            name_span,
        };
        if let Some(pratt) = Self::pratt_hint(node)? {
            decl.pratt = Some(pratt);
        }
        Ok(decl)
    }

    fn alt_body(&mut self, node: &KdlNode) -> Result<RuleKindSurf, SurfaceError> {
        let mut refs = Vec::new();
        for entry in positional(node) {
            let span = ByteSpan::from(entry.span());
            let name = as_bare(self.text, entry)?;
            refs.push((name.to_owned(), span));
        }
        if refs.is_empty() {
            return Err(SurfaceError::MissingArg {
                expected: "at least one alternative reference",
                span: span_of(node),
            });
        }
        Ok(RuleKindSurf::Alt { refs })
    }

    fn seq_body(&mut self, node: &KdlNode) -> Result<RuleKindSurf, SurfaceError> {
        let mut items: Vec<SeqEl> = Vec::new();
        for entry in positional(node) {
            let span = ByteSpan::from(entry.span());
            let KdlValue::String(value) = entry.value() else {
                return Err(SurfaceError::BadValue {
                    expected: "an identifier, a quoted literal, or a marker word",
                    span,
                });
            };
            let quoted = is_quoted(self.text, entry);
            if quoted {
                items.push(SeqEl {
                    item: ItemSurf::Lit {
                        text: value.clone(),
                    },
                    marker: None,
                    span,
                });
                continue;
            }
            if let Some(marker) = Marker::from_word(value) {
                let previous = items.last_mut().ok_or(SurfaceError::DanglingMarker {
                    marker: marker.word().to_owned(),
                    span,
                })?;
                if previous.marker.is_some() {
                    return Err(SurfaceError::DanglingMarker {
                        marker: marker.word().to_owned(),
                        span,
                    });
                }
                previous.marker = Some(marker);
                continue;
            }
            items.push(SeqEl {
                item: ItemSurf::Ref {
                    name: value.clone(),
                },
                marker: None,
                span,
            });
        }
        Ok(RuleKindSurf::Seq { items })
    }

    fn sep_body(&mut self, node: &KdlNode) -> Result<RuleKindSurf, SurfaceError> {
        let span = span_of(node);
        let element_entry = prop(node, "element").ok_or(SurfaceError::MissingArg {
            expected: "element=<rule name>",
            span,
        })?;
        let element_span = ByteSpan::from(element_entry.span());
        let element = as_bare(self.text, element_entry)?.to_owned();
        let separator_entry = prop(node, "separator").ok_or(SurfaceError::MissingArg {
            expected: "separator=<literal>",
            span,
        })?;
        let separator = as_string(separator_entry)?.to_owned();
        let allow_trailing = flag(node, "allow-trailing")?;
        Ok(RuleKindSurf::Sep(SepCfg {
            element,
            element_span,
            separator,
            allow_trailing,
        }))
    }

    fn pratt_hint(node: &KdlNode) -> Result<Option<gir_ir::PrattHint>, SurfaceError> {
        let lbp = match prop(node, "lbp") {
            Some(entry) => as_bp(entry)?,
            None => 0,
        };
        let explicit_rbp = match prop(node, "rbp") {
            Some(entry) => Some(as_bp(entry)?),
            None => None,
        };
        let assoc = match prop(node, "assoc") {
            Some(entry) => match as_string(entry)? {
                "left" => Some(gir_ir::Associativity::Left),
                "right" => Some(gir_ir::Associativity::Right),
                "none" => Some(gir_ir::Associativity::None),
                other => {
                    return Err(SurfaceError::BadPratt {
                        reason: format!("unknown associativity `{other}`"),
                        span: ByteSpan::from(entry.span()),
                    });
                }
            },
            None => None,
        };
        if lbp == 0 && explicit_rbp.is_none() && assoc.is_none() {
            return Ok(None);
        }
        if lbp == 0 {
            let span = prop(node, "lbp")
                .map_or_else(|| span_of(node), |entry| ByteSpan::from(entry.span()));
            return Err(SurfaceError::BadPratt {
                reason: "lbp must be at least 1 when a pratt hint is present".to_owned(),
                span,
            });
        }
        let assoc = assoc.unwrap_or(gir_ir::Associativity::Left);
        // The derived right binding power follows the descent engine's
        // documented infix encoding (`desc::InfixDesc`): left
        // associativity is `rbp = lbp + 1` (the same-level operator must
        // not join inside the right operand's climb), right/none is
        // `rbp = lbp`. An explicit `rbp=` on the surface overrides.
        let right_bp = explicit_rbp.unwrap_or(match assoc {
            gir_ir::Associativity::Left => lbp.saturating_add(1),
            gir_ir::Associativity::Right | gir_ir::Associativity::None => lbp,
        });
        Ok(Some(gir_ir::PrattHint {
            left_bp: lbp,
            right_bp,
            assoc,
        }))
    }
}

// ---------------------------------------------------------------------------
// Entry helpers
// ---------------------------------------------------------------------------

/// The byte span of a node (name through children).
fn span_of(node: &KdlNode) -> ByteSpan {
    ByteSpan::from(node.span())
}

/// All positional (unnamed) entries of a node.
fn positional(node: &KdlNode) -> impl Iterator<Item = &KdlEntry> {
    node.entries().iter().filter(|entry| entry.name().is_none())
}

/// A named property entry.
fn prop<'n>(node: &'n KdlNode, key: &str) -> Option<&'n KdlEntry> {
    node.entries()
        .iter()
        .find(|entry| entry.name().is_some_and(|name| name.value() == key))
}

/// All entries for a repeated property (`extension=... extension=...`).
fn prop_all<'n>(node: &'n KdlNode, key: &str) -> Vec<&'n KdlEntry> {
    node.entries()
        .iter()
        .filter(|entry| entry.name().is_some_and(|name| name.value() == key))
        .collect()
}

/// A boolean property (default false).
fn flag(node: &KdlNode, key: &str) -> Result<bool, SurfaceError> {
    match prop(node, key) {
        Some(entry) => as_bool(entry),
        None => Ok(false),
    }
}

fn as_string(entry: &KdlEntry) -> Result<&str, SurfaceError> {
    match entry.value() {
        KdlValue::String(text) => Ok(text),
        _ => Err(SurfaceError::BadValue {
            expected: "a string",
            span: ByteSpan::from(entry.span()),
        }),
    }
}

fn as_bool(entry: &KdlEntry) -> Result<bool, SurfaceError> {
    match entry.value() {
        KdlValue::Bool(value) => Ok(*value),
        // KDL v2 has no bare boolean literals: `flag="true"` is the
        // canonical spelling (bare `true`/`false` are reserved words).
        KdlValue::String(text) => match text.as_str() {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(SurfaceError::BadValue {
                expected: "a boolean (\"true\" or \"false\")",
                span: ByteSpan::from(entry.span()),
            }),
        },
        _ => Err(SurfaceError::BadValue {
            expected: "a boolean (\"true\" or \"false\")",
            span: ByteSpan::from(entry.span()),
        }),
    }
}

fn as_bp(entry: &KdlEntry) -> Result<u8, SurfaceError> {
    match entry.value() {
        KdlValue::Integer(value) => u8::try_from(*value).map_err(|_| SurfaceError::BadPratt {
            reason: format!("binding power {value} outside 0..=255"),
            span: ByteSpan::from(entry.span()),
        }),
        _ => Err(SurfaceError::BadValue {
            expected: "an integer binding power",
            span: ByteSpan::from(entry.span()),
        }),
    }
}

/// Whether the entry's source slice starts with a quote (distinguishes
/// `"{"` from a bare identifier — both are `KdlValue::String`).
fn is_quoted(text: &str, entry: &KdlEntry) -> bool {
    text.as_bytes()
        .get(entry.span().offset())
        .is_some_and(|b| *b == b'"')
}

/// A bare (unquoted) identifier argument.
fn as_bare<'a>(text: &'a str, entry: &'a KdlEntry) -> Result<&'a str, SurfaceError> {
    if is_quoted(text, entry) {
        return Err(SurfaceError::BadValue {
            expected: "a bare identifier",
            span: ByteSpan::from(entry.span()),
        });
    }
    as_string(entry)
}
