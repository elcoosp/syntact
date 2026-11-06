//! The document model (issue 041): one analyzed `.gir` document plus
//! the workspace that indexes a set of them. Every §9.4 feature answers
//! from here:
//!
//! - **symbols** — declarations (rules + tokens) and reference sites,
//!   all span-preserving, resolved against the declaration table.
//! - **hover** — signatures on declarations and references, annotation
//!   and marker documentation from a static table.
//! - **completion** — rule/token names plus the five `(seq)` markers.
//! - **conflict squiggles** — surface errors at their spans, LL(1)
//!   conflicts from the determinism report (downgraded on pratt rules),
//!   LR(1) shadow conflicts mapped through production origins, left
//!   recursion without a pratt hint, unreachable rules.
//! - **definition / references / rename** — per-file symbol identity
//!   (the surface is self-contained by construction: a reference must
//!   resolve in-file), workspace-indexed so any open grammar file is
//!   queryable. Rename rewrites every site in place — declaration order
//!   and therefore the dense `RuleId`/`TokenId` numbering never moves.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use gir_analysis::DeterminismReport;
use gir_ir::Grammar;
use gir_syntax::doc::{GirDoc, ItemSurf, RuleDecl, RuleKindSurf};
use gir_syntax::error::SurfaceError;
use gir_syntax::{ByteSpan, RESERVED_MARKERS};
use lr_shadow::lower::Look;
use lr_shadow::{Automaton, ConflictKind};
use serde::Serialize;

use crate::index::{LineIndex, Position, Range, word_at};

// ---------------------------------------------------------------------------
// Symbols
// ---------------------------------------------------------------------------

/// What kind of grammar symbol a declaration is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclKind {
    /// A `grammar` rule (nonterminal).
    Rule,
    /// A `tokens` declaration (terminal).
    Token,
}

/// One declaration: a rule or token with its name and node spans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decl {
    /// The declared name.
    pub name: String,
    /// Rule or token.
    pub kind: DeclKind,
    /// The name identifier span.
    pub name_span: ByteSpan,
    /// The whole declaration node span.
    pub full_span: ByteSpan,
}

/// One reference site: an occurrence of a declared name in a body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefSite {
    /// The referenced name (unresolved occurrences keep the raw text).
    pub name: String,
    /// The reference span.
    pub span: ByteSpan,
}

/// The symbol table of one document.
#[derive(Debug, Clone, Default)]
pub struct Symbols {
    /// Declarations in surface order (tokens first, then rules — the
    /// surface file order).
    pub decls: Vec<Decl>,
    /// Name → index into [`Self::decls`].
    decl_index: HashMap<String, usize>,
    /// Every reference site in surface order.
    pub refs: Vec<RefSite>,
}

impl Symbols {
    /// Build the table from a loaded surface document.
    #[must_use]
    pub fn of_doc(doc: &GirDoc) -> Self {
        let mut symbols = Self::default();
        for token in &doc.tokens {
            symbols.push_decl(&token.name, DeclKind::Token, token.name_span, token.span);
        }
        for rule in &doc.rules {
            symbols.push_decl(&rule.name, DeclKind::Rule, rule.name_span, rule.span);
            symbols.push_rule_refs(rule);
        }
        symbols
    }

    fn push_decl(&mut self, name: &str, kind: DeclKind, name_span: ByteSpan, span: ByteSpan) {
        let index = self.decls.len();
        self.decls_index(name, index);
        self.decls.push(Decl {
            name: name.to_owned(),
            kind,
            name_span,
            full_span: span,
        });
    }

    fn decls_index(&mut self, name: &str, index: usize) {
        self.decl_index.entry(name.to_owned()).or_insert(index);
    }

    fn push_rule_refs(&mut self, rule: &RuleDecl) {
        match &rule.kind {
            RuleKindSurf::Alt { refs } => {
                for (name, span) in refs {
                    self.refs.push(RefSite {
                        name: name.clone(),
                        span: *span,
                    });
                }
            }
            RuleKindSurf::Seq { items } => {
                for item in items {
                    if let ItemSurf::Ref { name } = &item.item {
                        self.refs.push(RefSite {
                            name: name.clone(),
                            span: item.span,
                        });
                    }
                }
            }
            RuleKindSurf::Sep(cfg) => {
                self.refs.push(RefSite {
                    name: cfg.element.clone(),
                    span: cfg.element_span,
                });
            }
        }
    }

    /// The declaration with this exact name.
    #[must_use]
    pub fn decl(&self, name: &str) -> Option<&Decl> {
        self.decl_index
            .get(name)
            .copied()
            .map(|index| &self.decls[index])
    }

    /// The declaration whose name span contains `offset`.
    #[must_use]
    pub fn decl_at(&self, offset: usize) -> Option<&Decl> {
        self.decls
            .iter()
            .find(|decl| decl.name_span.contains(offset))
    }

    /// The reference site whose span contains `offset`.
    #[must_use]
    pub fn ref_at(&self, offset: usize) -> Option<&RefSite> {
        self.refs.iter().find(|site| site.span.contains(offset))
    }

    /// Resolve a reference to its declaration (same-file identity).
    #[must_use]
    pub fn resolve(&self, site: &RefSite) -> Option<&Decl> {
        self.decl(&site.name)
    }

    /// All reference sites of a declaration (the sites whose name
    /// matches; same-file identity).
    #[must_use]
    pub fn refs_of(&self, decl: &Decl) -> Vec<&RefSite> {
        self.refs
            .iter()
            .filter(|site| site.name == decl.name)
            .collect()
    }
}

/// Byte-span helpers used across the analysis (a local extension
/// trait — `gir_syntax::ByteSpan` is a foreign type).
pub trait SpanExt {
    /// Whether `offset` lies within the span (end-exclusive; an empty
    /// span contains its own start point).
    fn contains(self, offset: usize) -> bool;
}

impl SpanExt for ByteSpan {
    fn contains(self, offset: usize) -> bool {
        offset >= self.start && offset < self.end.max(self.start + 1)
    }
}

// ---------------------------------------------------------------------------
// Diagnostics and feature payloads
// ---------------------------------------------------------------------------

/// LSP diagnostic severity (the protocol's numeric values).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagSeverity {
    /// 1 — breaks the build.
    Error = 1,
    /// 2 — a determinism fallback or ambiguity.
    Warning = 2,
    /// 3 — informational (expected conflicts, shadow notes).
    Information = 3,
    /// 4 — hints (dead rules).
    Hint = 4,
}

impl serde::Serialize for DiagSeverity {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(*self as u8)
    }
}

/// One inline squiggle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diag {
    /// Where the squiggle sits.
    pub range: Range,
    /// Severity.
    pub severity: DiagSeverity,
    /// The message.
    pub message: String,
}

/// A location in a workspace file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Location {
    /// The document uri.
    pub uri: String,
    /// The range inside it.
    pub range: Range,
}

/// A hover: markdown contents plus the highlighted range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Hover {
    /// Markdown contents (`{"kind":"markdown","value":…}`).
    pub contents: HoverContents,
    /// The range the hover applies to.
    pub range: Range,
}

/// Markdown hover contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HoverContents {
    /// Always `"markdown"`.
    pub kind: &'static str,
    /// The markdown body.
    pub value: String,
}

/// A completion item (the subset the grammar server needs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompletionItem {
    /// The inserted text.
    pub label: String,
    /// LSP `CompletionItemKind` (3 rule, 14 token, 24 marker).
    pub kind: u8,
    /// Short qualifier (`rule` / `token` / `seq marker`) — a static
    /// label, so completion items cost one allocation each (the label).
    pub detail: &'static str,
    /// Markdown documentation (markers only).
    pub documentation: String,
}

/// Completion item kinds (LSP numeric values).
pub const COMPLETION_RULE: u8 = 3;
/// See [`COMPLETION_RULE`].
pub const COMPLETION_TOKEN: u8 = 14;
/// See [`COMPLETION_RULE`].
pub const COMPLETION_MARKER: u8 = 24;

/// One text edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextEdit {
    /// Where to replace.
    pub range: Range,
    /// The replacement text.
    pub new_text: String,
}

/// A workspace edit: per-uri edit lists, uris in deterministic order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceEdit {
    /// Edits grouped by document uri.
    pub changes: BTreeMap<String, Vec<TextEdit>>,
}

/// A document symbol (flat: rules + tokens).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolInfo {
    /// The symbol name.
    pub name: String,
    /// LSP `SymbolKind` (12 rule, 13 token).
    pub kind: u8,
    /// The whole declaration range.
    pub range: Range,
    /// The name selection range.
    pub selection_range: Range,
}

/// Symbol kinds (LSP numeric values).
pub const SYMBOL_RULE: u8 = 12;
/// See [`SYMBOL_RULE`].
pub const SYMBOL_TOKEN: u8 = 13;

/// Why a rename was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameError {
    /// The cursor is not on a declaration or reference.
    NoSymbol,
    /// The new name is not a legal surface identifier.
    BadName,
    /// The new name is a reserved `(seq)` marker word.
    Reserved,
    /// Another declaration in this file already uses the name.
    Duplicate,
}

// ---------------------------------------------------------------------------
// Annotation documentation (hover table)
// ---------------------------------------------------------------------------

/// Hover documentation for an annotation word (property keys, `kind`
/// values, and the backward-binding `(seq)` markers).
#[must_use]
pub fn annotation_doc(word: &str) -> Option<&'static str> {
    Some(match word {
        "language" => {
            "The `language` node: name, `version`, `extension` list, `unicode` lexing flag. Must appear once."
        }
        "version" => {
            "Language version string (`version=\"1.0.0\"`). Rides the generated crate metadata."
        }
        "extension" => "File extension mapped to this grammar (without the dot; repeatable).",
        "unicode" => "`unicode=\"true\"` switches the lexer to Unicode character classes.",
        "tokens" => "The token declarations: terminals matched by the generated lexer.",
        "grammar" => {
            "The rule declarations: nonterminals of the grammar. The first rule is the start rule."
        }
        "kind" => {
            "The body kind: `alt` (ordered choice), `seq` (sequence), `sep` (separated list sugar), `lit`/`re` for tokens."
        }
        "lit" => "Fixed literal text — the token matches exactly this string.",
        "re" => "A regular expression, compiled by the lexer generator (regex → NFA → DFA).",
        "node" => {
            "`node=\"true\"` emits a named tree node for this rule/token (tree-visible structure)."
        }
        "reuse" => {
            "`reuse=\"true\"` marks an incremental-reuse boundary: the node is parse-state-stamped for reparsing (issue 030)."
        }
        "trivia" => {
            "`trivia=\"true\"` marks a whitespace/comment token — never a token event, rides trivia records (ADR-008)."
        }
        "wrap-error" => {
            "`wrap-error=\"true\"` lets recovery wrap offending regions of this rule in ERROR nodes."
        }
        "lbp" => {
            "Pratt left binding power. A rule with `lbp`/`rbp` lowers to precedence climbing (issue 023)."
        }
        "rbp" => {
            "Pratt right binding power. A rule with `lbp`/`rbp` lowers to precedence climbing (issue 023)."
        }
        "assoc" => "Pratt associativity: `left`, `right`, or `none` (flat comparisons).",
        "index-bound" => {
            "Structural-index boundary string: the scanner treats runs delimited by this byte as opaque regions (issue 032)."
        }
        "allow-trailing" => {
            "`allow-trailing=\"true\"` permits one trailing separator in a `sep` list."
        }
        "element" => "The `sep` list element (a rule or token name).",
        "separator" => "The `sep` list separator literal.",
        "opt" => "`opt` binds backward: the previous element becomes optional (zero or one).",
        "rep" => "`rep` binds backward: the previous element repeats zero or more times.",
        "rep1" => "`rep1` binds backward: the previous element repeats one or more times.",
        "and" => {
            "`and` binds backward: the previous element becomes a positive lookahead (matches without consuming)."
        }
        "not" => {
            "`not` binds backward: the previous element becomes a negative lookahead (succeeds when the inner fails)."
        }
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Document
// ---------------------------------------------------------------------------

/// One analyzed `.gir` document: raw text, position index, surface
/// symbols, and the full diagnostic set.
#[derive(Debug, Clone)]
pub struct Document {
    /// The current text.
    pub text: String,
    /// The position index for the current text.
    pub line_index: LineIndex,
    /// The loaded surface document (`None` while the text does not
    /// parse).
    pub surface: Option<GirDoc>,
    /// The symbol table (empty while the text does not parse).
    pub symbols: Symbols,
    /// Surface errors + analysis diagnostics, in a stable order.
    pub diagnostics: Vec<Diag>,
}

impl Document {
    /// Analyze a freshly opened document.
    #[must_use]
    pub fn open(text: String) -> Self {
        let mut document = Self {
            line_index: LineIndex::new(""),
            text: String::new(),
            surface: None,
            symbols: Symbols::default(),
            diagnostics: Vec::new(),
        };
        document.reanalyze(text);
        document
    }

    /// Apply one change (full text when `span` is `None`) and
    /// re-analyze.
    pub fn edit(&mut self, span: Option<ByteSpan>, new_text: &str) {
        let text = match span {
            None => new_text.to_owned(),
            Some(span) => {
                let mut text = self.text.clone();
                let start = span.start.min(text.len());
                let end = span.end.clamp(start, text.len());
                text.replace_range(start..end, new_text);
                text
            }
        };
        self.reanalyze(text);
    }

    fn reanalyze(&mut self, text: String) {
        self.line_index = LineIndex::new(&text);
        self.text = text;
        self.diagnostics.clear();
        match gir_syntax::load(&self.text) {
            Err(error) => {
                self.surface = None;
                self.symbols = Symbols::default();
                let span = error_span(&error);
                self.diagnostics.push(Diag {
                    range: self.line_index.range(&self.text, span),
                    severity: DiagSeverity::Error,
                    message: error.to_string(),
                });
            }
            Ok(surface) => {
                self.symbols = Symbols::of_doc(&surface);
                self.surface = Some(surface);
                self.analyze_grammar();
            }
        }
    }

    fn analyze_grammar(&mut self) {
        let found = self.collect_grammar_diagnostics();
        let text = self.text.clone();
        self.diagnostics
            .extend(found.into_iter().map(|(span, severity, message)| Diag {
                range: self.line_index.range(&text, span),
                severity,
                message,
            }));
    }

    /// The grammar-level diagnostics (immutable pass; spans resolve
    /// against the symbol table collected from the surface).
    fn collect_grammar_diagnostics(&self) -> Vec<(ByteSpan, DiagSeverity, String)> {
        let Some(surface) = &self.surface else {
            return Vec::new();
        };
        let mut found: Vec<(ByteSpan, DiagSeverity, String)> = Vec::new();
        let span_of = |name: &str| {
            self.symbols
                .decl(name)
                .map_or_else(|| ByteSpan::empty(0), |decl| decl.name_span)
        };

        let grammar = match surface.to_grammar() {
            Ok(grammar) => grammar,
            Err(error) => {
                // Reference/shape errors surface with their real span
                // and message (the loader validates structure, lowering
                // validates name resolution and IR shape).
                found.push((error_span(&error), DiagSeverity::Error, error.to_string()));
                return found;
            }
        };

        let report = DeterminismReport::compute(&grammar);
        Self::determinism_diagnostics(&grammar, &report, &span_of, &mut found);
        // Shadow LR(1) conflicts (roadmap §9.4 "queries lr-shadow"):
        // recorded ambiguities, informational.
        Self::shadow_diagnostics(&grammar, &span_of, &mut found);
        found
    }

    /// Determinism-report diagnostics: left recursion, unreachable
    /// rules, LL(1) conflict sites.
    fn determinism_diagnostics(
        grammar: &Grammar,
        report: &DeterminismReport,
        span_of: &impl Fn(&str) -> ByteSpan,
        found: &mut Vec<(ByteSpan, DiagSeverity, String)>,
    ) {
        // Left recursion without a pratt hint is a hard error.
        for verdict in &report.verdicts {
            let Some(rule) = grammar.try_rule(verdict.rule) else {
                continue;
            };
            let name = grammar.interner.lookup(rule.name).to_owned();
            if verdict.left_recursive && rule.pratt.is_none() {
                found.push((
                    span_of(&name),
                    DiagSeverity::Error,
                    format!(
                        "rule `{name}` is left-recursive without a pratt hint; the descent backend rejects it"
                    ),
                ));
            }
            if !verdict.reachable {
                found.push((
                    span_of(&name),
                    DiagSeverity::Hint,
                    format!("rule `{name}` is unreachable from the start rule"),
                ));
            }
        }
        // LL(1) conflicts: warnings, downgraded on pratt rules where
        // conflicts are the expected shape of an expression region.
        for site in &report.conflicts {
            let Some(rule) = grammar.try_rule(site.rule) else {
                continue;
            };
            let name = grammar.interner.lookup(rule.name).to_owned();
            let token = grammar.token_name(site.token).to_owned();
            let (severity, note) = if rule.pratt.is_some() {
                (
                    DiagSeverity::Information,
                    "expected inside a pratt expression region",
                )
            } else {
                (
                    DiagSeverity::Warning,
                    "the table backend takes over for this rule",
                )
            };
            found.push((
                span_of(&name),
                severity,
                format!(
                    "LL(1) conflict in rule `{name}`: branches {} and {} both start with token `{token}` — {note}",
                    site.branches.0, site.branches.1
                ),
            ));
        }
    }

    /// LR(1) shadow-automaton diagnostics: recorded (resolved)
    /// ambiguities, deduplicated per rule and kind.
    fn shadow_diagnostics(
        grammar: &Grammar,
        span_of: &impl Fn(&str) -> ByteSpan,
        found: &mut Vec<(ByteSpan, DiagSeverity, String)>,
    ) {
        let automaton = Automaton::build(grammar);
        let mut seen: BTreeSet<(String, &'static str)> = BTreeSet::new();
        for conflict in &automaton.conflicts {
            let Some(origin) = origin_rule(&automaton, conflict.state) else {
                continue;
            };
            let Some(rule) = grammar.try_rule(origin) else {
                continue;
            };
            let name = grammar.interner.lookup(rule.name).to_owned();
            let kind: &'static str = match conflict.kind {
                ConflictKind::ShiftReduce => "shift/reduce",
                ConflictKind::ReduceReduce => "reduce/reduce",
            };
            if !seen.insert((name.clone(), kind)) {
                continue;
            }
            let term = match conflict.term {
                Look::EOF => "eof".to_owned(),
                other => grammar
                    .try_token(other.as_token().expect("non-EOF looks are tokens"))
                    .map_or_else(
                        || "tok".to_owned(),
                        |token| grammar.interner.lookup(token.name).to_owned(),
                    ),
            };
            let resolution = match conflict.kind {
                ConflictKind::ShiftReduce => "resolved by shift",
                ConflictKind::ReduceReduce => "resolved by declaration order",
            };
            found.push((
                span_of(&name),
                DiagSeverity::Information,
                format!(
                    "LR(1) shadow: {kind} conflict on token `{term}` in rule `{name}` ({resolution})"
                ),
            ));
        }
    }

    // -----------------------------------------------------------------
    // Features (byte-offset API; the server maps LSP positions)
    // -----------------------------------------------------------------

    /// The declaration or referenced declaration at `offset`.
    #[must_use]
    pub fn symbol_at(&self, offset: usize) -> Option<&Decl> {
        if let Some(decl) = self.symbols.decl_at(offset) {
            return Some(decl);
        }
        let site = self.symbols.ref_at(offset)?;
        self.symbols.resolve(site)
    }

    /// Hover at `offset`.
    #[must_use]
    pub fn hover_at(&self, offset: usize) -> Option<Hover> {
        let span = word_at(&self.text, offset)?;
        let word = span.slice(&self.text);
        let contents = self
            .symbol_hover(offset, word)
            .or_else(|| annotation_doc(word).map(|doc| format!("`{word}` — {doc}")))?;
        Some(Hover {
            contents: HoverContents {
                kind: "markdown",
                value: contents,
            },
            range: self.line_index.range(&self.text, span),
        })
    }

    fn symbol_hover(&self, offset: usize, word: &str) -> Option<String> {
        let (decl, is_ref) = if let Some(decl) = self.symbols.decl_at(offset) {
            (decl, false)
        } else {
            let site = self.symbols.ref_at(offset)?;
            (self.symbols.resolve(site)?, true)
        };
        if decl.name != word {
            return None;
        }
        let what = match decl.kind {
            DeclKind::Rule => "rule",
            DeclKind::Token => "token",
        };
        let signature = decl.full_span.slice(&self.text);
        let mut value = format!("{what} `{}`\n\n```kdl\n{}\n```", decl.name, signature);
        if is_ref {
            value.push_str("\n\n*References the declaration above; rename updates every site in place and never reorders declarations.*");
        }
        Some(value)
    }

    /// Completion items at `offset` (all symbols + markers, filtered
    /// by the word prefix under the cursor).
    #[must_use]
    pub fn completion_at(&self, offset: usize) -> Vec<CompletionItem> {
        let prefix = word_at(&self.text, offset)
            .map_or_else(String::new, |span| span.slice(&self.text).to_owned());
        // Pre-sized: the list is (almost always) unfiltered, so growth
        // reallocations would dominate its allocation profile.
        let mut items = Vec::with_capacity(self.symbols.decls.len() + RESERVED_MARKERS.len());
        for decl in &self.symbols.decls {
            if !decl.name.starts_with(&prefix) {
                continue;
            }
            let (kind, detail) = match decl.kind {
                DeclKind::Rule => (COMPLETION_RULE, "rule"),
                DeclKind::Token => (COMPLETION_TOKEN, "token"),
            };
            items.push(CompletionItem {
                label: decl.name.clone(),
                kind,
                detail,
                documentation: String::new(),
            });
        }
        for marker in RESERVED_MARKERS {
            if marker.starts_with(&prefix) {
                items.push(CompletionItem {
                    label: marker.to_owned(),
                    kind: COMPLETION_MARKER,
                    detail: "seq marker",
                    documentation: annotation_doc(marker).unwrap_or_default().to_owned(),
                });
            }
        }
        items
    }

    /// The declaration name span that `offset`'s reference points to.
    #[must_use]
    pub fn definition_at(&self, offset: usize) -> Option<ByteSpan> {
        if self.symbols.decl_at(offset).is_some() {
            return None; // already at the definition
        }
        let site = self.symbols.ref_at(offset)?;
        Some(self.symbols.resolve(site)?.name_span)
    }

    /// Every reference span of the symbol at `offset` (plus the
    /// declaration itself when `include_decl`).
    #[must_use]
    pub fn references_at(&self, offset: usize, include_decl: bool) -> Vec<ByteSpan> {
        let Some(decl) = self.symbol_at(offset) else {
            return Vec::new();
        };
        let mut spans = Vec::new();
        if include_decl {
            spans.push(decl.name_span);
        }
        spans.extend(self.symbols.refs_of(decl).iter().map(|site| site.span));
        spans
    }

    /// Rename the symbol at `offset`: validate the name, return the
    /// byte spans to rewrite.
    ///
    /// # Errors
    /// [`RenameError`] with the reason (see the variant docs).
    pub fn rename_at(&self, offset: usize, new_name: &str) -> Result<Vec<ByteSpan>, RenameError> {
        let decl = self.symbol_at(offset).ok_or(RenameError::NoSymbol)?;
        valid_name(new_name)?;
        if let Some(existing) = self.symbols.decl(new_name) {
            if existing.name != decl.name {
                return Err(RenameError::Duplicate);
            }
        }
        let mut spans = vec![decl.name_span];
        spans.extend(self.symbols.refs_of(decl).iter().map(|site| site.span));
        Ok(spans)
    }

    /// Document symbols: tokens then rules, surface order.
    #[must_use]
    pub fn document_symbols(&self) -> Vec<SymbolInfo> {
        self.symbols
            .decls
            .iter()
            .map(|decl| SymbolInfo {
                name: decl.name.clone(),
                kind: match decl.kind {
                    DeclKind::Rule => SYMBOL_RULE,
                    DeclKind::Token => SYMBOL_TOKEN,
                },
                range: self.line_index.range(&self.text, decl.full_span),
                selection_range: self.line_index.range(&self.text, decl.name_span),
            })
            .collect()
    }
}

/// Validate a rename target (surface identifier, not a marker).
fn valid_name(name: &str) -> Result<(), RenameError> {
    let mut chars = name.chars();
    let first_ok = chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_');
    let rest_ok = name
        .bytes()
        .skip(1)
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
    if first_ok && rest_ok {
        return if RESERVED_MARKERS.contains(&name) {
            Err(RenameError::Reserved)
        } else {
            Ok(())
        };
    }
    Err(RenameError::BadName)
}

fn error_span(error: &SurfaceError) -> ByteSpan {
    match error {
        SurfaceError::Kdl { span, .. }
        | SurfaceError::UnknownSection { span, .. }
        | SurfaceError::DuplicateLanguage { span }
        | SurfaceError::MissingLanguageName { span }
        | SurfaceError::DuplicateName { span, .. }
        | SurfaceError::ReservedName { span, .. }
        | SurfaceError::UnknownKind { span, .. }
        | SurfaceError::BadValue { span, .. }
        | SurfaceError::MissingArg { span, .. }
        | SurfaceError::DanglingMarker { span, .. }
        | SurfaceError::UnknownRef { span, .. }
        | SurfaceError::BadPratt { span, .. }
        | SurfaceError::Ir { span, .. } => *span,
        SurfaceError::MissingLanguage => ByteSpan::empty(0),
    }
}

/// The GIR rule a shadow-automaton state lowers from (via the kernel
/// items' production origins).
fn origin_rule(automaton: &Automaton, state: lr_shadow::StateId) -> Option<gir_ir::RuleId> {
    let state_ref = automaton.states.get(state.0 as usize)?;
    let item = state_ref.kernel.iter().next()?;
    let production = automaton.lowered.productions.get(item.prod as usize)?;
    production.origin
}

// ---------------------------------------------------------------------------
// Workspace
// ---------------------------------------------------------------------------

/// The workspace: every open document, uri-ordered (deterministic
/// iteration for cross-file answers and rename edits).
#[derive(Debug, Default)]
pub struct Workspace {
    documents: BTreeMap<String, Document>,
}

impl Workspace {
    /// An empty workspace.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Open (or replace) a document.
    pub fn open(&mut self, uri: &str, text: String) {
        self.documents.insert(uri.to_owned(), Document::open(text));
    }

    /// Apply incremental edits; returns `false` for an unknown uri.
    pub fn change(&mut self, uri: &str, edits: &[(Option<ByteSpan>, &str)]) -> bool {
        let Some(document) = self.documents.get_mut(uri) else {
            return false;
        };
        for (span, new_text) in edits {
            document.edit(*span, new_text);
        }
        true
    }

    /// Close a document; returns `false` for an unknown uri.
    pub fn close(&mut self, uri: &str) -> bool {
        self.documents.remove(uri).is_some()
    }

    /// The document behind a uri.
    #[must_use]
    pub fn get(&self, uri: &str) -> Option<&Document> {
        self.documents.get(uri)
    }

    /// Open document uris, sorted.
    #[must_use]
    pub fn uris(&self) -> Vec<String> {
        self.documents.keys().cloned().collect()
    }

    fn location(&self, uri: &str, span: ByteSpan) -> Option<Location> {
        let document = self.documents.get(uri)?;
        Some(Location {
            uri: uri.to_owned(),
            range: document.line_index.range(&document.text, span),
        })
    }

    /// Hover in one document.
    #[must_use]
    pub fn hover(&self, uri: &str, offset: usize) -> Option<Hover> {
        self.documents.get(uri)?.hover_at(offset)
    }

    /// Completion in one document.
    #[must_use]
    pub fn completion(&self, uri: &str, offset: usize) -> Vec<CompletionItem> {
        self.documents
            .get(uri)
            .map_or_else(Vec::new, |document| document.completion_at(offset))
    }

    /// Document symbols.
    #[must_use]
    pub fn document_symbols(&self, uri: &str) -> Vec<SymbolInfo> {
        self.documents
            .get(uri)
            .map_or_else(Vec::new, Document::document_symbols)
    }

    /// Diagnostics of one document.
    #[must_use]
    pub fn diagnostics(&self, uri: &str) -> Vec<Diag> {
        self.documents
            .get(uri)
            .map_or_else(Vec::new, |document| document.diagnostics.clone())
    }

    /// Go-to-definition (same-file identity; `None` at a declaration).
    #[must_use]
    pub fn definition(&self, uri: &str, offset: usize) -> Option<Location> {
        let span = self.documents.get(uri)?.definition_at(offset)?;
        self.location(uri, span)
    }

    /// Find references across the workspace. Symbol identity is
    /// per-file, so the answer is the declaration's file's sites —
    /// every open grammar file stays independently queryable.
    #[must_use]
    pub fn references(&self, uri: &str, offset: usize, include_decl: bool) -> Vec<Location> {
        let Some(document) = self.documents.get(uri) else {
            return Vec::new();
        };
        document
            .references_at(offset, include_decl)
            .into_iter()
            .filter_map(|span| self.location(uri, span))
            .collect()
    }

    /// Rename across the workspace: the touched document's sites plus
    /// (for cross-file workspaces) nothing else — a `.gir` file is a
    /// closed grammar package, and per-file identity keeps renames
    /// deterministic under same-name declarations in sibling packs.
    ///
    /// # Errors
    /// [`RenameError`] mapped to a string message by the server.
    pub fn rename(
        &self,
        uri: &str,
        offset: usize,
        new_name: &str,
    ) -> Result<WorkspaceEdit, RenameError> {
        let document = self.documents.get(uri).ok_or(RenameError::NoSymbol)?;
        let spans = document.rename_at(offset, new_name)?;
        let index = &document.line_index;
        let text = &document.text;
        let edits: Vec<TextEdit> = spans
            .into_iter()
            .map(|span| TextEdit {
                range: index.range(text, span),
                new_text: new_name.to_owned(),
            })
            .collect();
        let mut changes = BTreeMap::new();
        changes.insert(uri.to_owned(), edits);
        Ok(WorkspaceEdit { changes })
    }

    /// Position helper for tests and callers holding LSP positions.
    #[must_use]
    pub fn offset_of(&self, uri: &str, position: Position) -> Option<usize> {
        let document = self.documents.get(uri)?;
        document.line_index.offset(&document.text, position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// No conflicts, one unreachable rule, refs to resolve.
    const MINI: &str = "\
language \"mini\" version=\"1.0.0\" extension=\"mini\"

tokens {
    number kind=re \"[0-9]+\"
    plus kind=lit \"+\"
}

grammar {
    expr   kind=alt number sum node=\"true\"
    sum    kind=seq plus expr
    unused kind=seq number
}
";

    /// The classic dangling-else ambiguity: an LL(1) conflict on `stmt`
    /// (both branches start with `iif`) plus a recorded shadow
    /// shift/reduce conflict resolved by shift.
    const CONFLICT: &str = "\
language \"amb\" version=\"1.0.0\"
tokens {
    iif kind=lit \"if\"
    else kind=lit \"else\"
    n kind=re \"[0-9]+\"
}
grammar {
    stmt kind=alt if-else if-only
    if-else kind=seq iif stmt else stmt
    if-only kind=seq iif stmt opt
}
";

    /// A pratt-hinted rule whose conflict is expected (unquoted
    /// integer binding powers).
    const PRATT: &str = "\
language \"pr\" version=\"1.0.0\"
tokens {
    n kind=re \"[0-9]+\"
    plus kind=lit \"+\"
}
grammar {
    e kind=alt a b lbp=1 rbp=2 assoc=left
    a kind=seq n
    b kind=seq n plus
}
";

    /// A surface error: `list` names nothing declared.
    const BROKEN: &str = "\
language \"brk\" version=\"1.0.0\"
grammar {
    s kind=alt list
}
";

    /// Indirect left recursion without a pratt hint.
    const LEFTREC: &str = "\
language \"lr\" version=\"1.0.0\"
tokens {
    n kind=re \"[0-9]+\"
}
grammar {
    a kind=alt b n
    b kind=seq a n
}
";

    /// A rename target with a reference site.
    const RENAME: &str = "\
language \"ren\" version=\"1.0.0\"
tokens {
    n kind=re \"[0-9]+\"
}
grammar {
    value kind=alt n list
    list kind=seq value rep
}
";

    fn byte_offset(text: &str, needle: &str) -> usize {
        text.find(needle).expect("fixture contains needle")
    }

    // -----------------------------------------------------------------
    // Symbols
    // -----------------------------------------------------------------

    #[test]
    fn symbols_collect_declarations_in_surface_order() {
        let document = Document::open(MINI.to_owned());
        let names: Vec<(&str, DeclKind)> = document
            .symbols
            .decls
            .iter()
            .map(|decl| (decl.name.as_str(), decl.kind))
            .collect();
        assert_eq!(
            names,
            vec![
                ("number", DeclKind::Token),
                ("plus", DeclKind::Token),
                ("expr", DeclKind::Rule),
                ("sum", DeclKind::Rule),
                ("unused", DeclKind::Rule),
            ]
        );
    }

    #[test]
    fn symbols_collect_reference_sites_with_spans() {
        let document = Document::open(MINI.to_owned());
        let names: Vec<&str> = document
            .symbols
            .refs
            .iter()
            .map(|site| site.name.as_str())
            .collect();
        assert_eq!(names, vec!["number", "sum", "plus", "expr", "number"]);
        // Every ref span slices to its name in the source.
        for site in &document.symbols.refs {
            assert_eq!(site.span.slice(MINI), site.name);
        }
    }

    #[test]
    fn symbol_at_resolves_declarations_and_references() {
        let document = Document::open(MINI.to_owned());
        let at_decl = byte_offset(MINI, "expr  ") + 1;
        assert_eq!(document.symbol_at(at_decl).expect("decl").name, "expr");
        let at_ref = MINI.find("sum ").expect("ref site") + 1;
        let resolved = document.symbol_at(at_ref).expect("resolves");
        assert_eq!(resolved.name, "sum");
        assert_eq!(resolved.kind, DeclKind::Rule);
    }

    // -----------------------------------------------------------------
    // Diagnostics
    // -----------------------------------------------------------------

    #[test]
    fn clean_grammar_has_no_diagnostics() {
        let document = Document::open(MINI.to_owned());
        // `unused` produces exactly one hint.
        assert_eq!(document.diagnostics.len(), 1);
        assert_eq!(document.diagnostics[0].severity, DiagSeverity::Hint);
        assert!(document.diagnostics[0].message.contains("unreachable"));
    }

    #[test]
    fn surface_errors_become_error_diagnostics_at_their_spans() {
        let document = Document::open(BROKEN.to_owned());
        assert_eq!(document.diagnostics.len(), 1);
        let diag = &document.diagnostics[0];
        assert_eq!(diag.severity, DiagSeverity::Error);
        assert!(diag.message.contains("list"), "{}", diag.message);
        // The squiggle sits on the offending reference.
        let expected = document.line_index.range(&document.text, {
            let start = BROKEN.find("list").expect("present");
            ByteSpan {
                start,
                end: start + 4,
            }
        });
        assert_eq!(diag.range, expected);
    }

    #[test]
    fn ll1_conflicts_squiggle_the_rule_with_backend_note() {
        let document = Document::open(CONFLICT.to_owned());
        let warning = document
            .diagnostics
            .iter()
            .find(|diag| diag.severity == DiagSeverity::Warning)
            .expect("conflict warning");
        assert!(
            warning.message.contains("LL(1) conflict"),
            "{}",
            warning.message
        );
        assert!(
            warning.message.contains("table backend"),
            "{}",
            warning.message
        );
        // The squiggle is on the `stmt` rule name.
        let stmt_range = document.line_index.range(&document.text, {
            let start = CONFLICT.find("stmt kind=alt").expect("present");
            ByteSpan {
                start,
                end: start + 4,
            }
        });
        assert_eq!(warning.range, stmt_range);
    }

    #[test]
    fn pratt_conflicts_are_informational() {
        let document = Document::open(PRATT.to_owned());
        let conflict = document
            .diagnostics
            .iter()
            .find(|diag| diag.message.contains("LL(1) conflict"))
            .expect("conflict present");
        assert_eq!(conflict.severity, DiagSeverity::Information);
        assert!(conflict.message.contains("pratt"), "{}", conflict.message);
    }

    #[test]
    fn left_recursion_without_pratt_is_an_error() {
        let document = Document::open(LEFTREC.to_owned());
        let error = document
            .diagnostics
            .iter()
            .find(|diag| diag.severity == DiagSeverity::Error)
            .expect("left-recursion error");
        assert!(
            error.message.contains("left-recursive"),
            "{}",
            error.message
        );
    }

    #[test]
    fn shadow_lr_conflicts_appear_as_information() {
        let document = Document::open(CONFLICT.to_owned());
        let shadow = document
            .diagnostics
            .iter()
            .find(|diag| diag.message.contains("LR(1) shadow"))
            .expect("shadow conflict");
        assert_eq!(shadow.severity, DiagSeverity::Information);
        assert!(
            shadow.message.contains("shift/reduce"),
            "{}",
            shadow.message
        );
        assert!(shadow.message.contains("`else`"), "{}", shadow.message);
        assert!(shadow.message.contains("`if-else`"), "{}", shadow.message);
        assert!(
            shadow.message.contains("resolved by shift"),
            "{}",
            shadow.message
        );
    }

    // -----------------------------------------------------------------
    // Hover
    // -----------------------------------------------------------------

    #[test]
    fn hover_on_declaration_shows_signature() {
        let document = Document::open(MINI.to_owned());
        let offset = byte_offset(MINI, "sum    kind=seq") + 1;
        let hover = document.hover_at(offset).expect("hover");
        assert!(
            hover.contents.value.contains("rule `sum`"),
            "{}",
            hover.contents.value
        );
        assert!(
            hover.contents.value.contains("kind=seq"),
            "{}",
            hover.contents.value
        );
        assert_eq!(hover.contents.kind, "markdown");
        // The hover range covers the hovered word (`sum` sits at
        // character 4 after the indent).
        assert_eq!(hover.range.start.character, 4);
    }

    #[test]
    fn hover_on_reference_explains_the_target() {
        let document = Document::open(MINI.to_owned());
        let offset = MINI.find("sum ").expect("ref") + 1;
        let hover = document.hover_at(offset).expect("hover");
        assert!(
            hover.contents.value.contains("rule `sum`"),
            "{}",
            hover.contents.value
        );
        assert!(
            hover.contents.value.contains("rename updates every site"),
            "{}",
            hover.contents.value
        );
    }

    #[test]
    fn hover_on_annotation_and_marker_words() {
        let document = Document::open(MINI.to_owned());
        let node_offset = MINI.find("node=\"true\"").expect("annotation");
        let hover = document
            .hover_at(node_offset + 1)
            .expect("annotation hover");
        assert!(
            hover.contents.value.contains("`node`"),
            "{}",
            hover.contents.value
        );
        assert!(
            hover.contents.value.contains("named tree node"),
            "{}",
            hover.contents.value
        );

        let rep_doc = Document::open(RENAME.to_owned());
        let rep_offset = RENAME.find(" rep").expect("marker") + 2;
        let hover = rep_doc.hover_at(rep_offset).expect("marker hover");
        assert!(
            hover.contents.value.contains("`rep`"),
            "{}",
            hover.contents.value
        );
        assert!(
            hover.contents.value.contains("zero or more"),
            "{}",
            hover.contents.value
        );
    }

    #[test]
    fn hover_misses_return_none() {
        let document = Document::open(MINI.to_owned());
        // The `{` of the tokens block: no word, no annotation.
        let braces = MINI.find('{').expect("brace");
        assert_eq!(document.hover_at(braces), None);
        // Whitespace just before the `tokens` section.
        let gap = MINI.find("tokens").expect("section") - 1;
        assert_eq!(document.hover_at(gap), None);
    }

    // -----------------------------------------------------------------
    // Completion
    // -----------------------------------------------------------------

    #[test]
    fn completion_lists_rules_tokens_and_markers() {
        let document = Document::open(MINI.to_owned());
        // On the closing brace of the grammar block (pure punctuation
        // under the cursor): the unfiltered list.
        let offset = MINI.rfind('}').expect("closing brace");
        let items = document.completion_at(offset);
        let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
        assert!(labels.contains(&"expr"));
        assert!(labels.contains(&"sum"));
        assert!(labels.contains(&"number"));
        assert!(labels.contains(&"opt"));
        assert!(labels.contains(&"rep1"));
        let marker = items
            .iter()
            .find(|item| item.label == "opt")
            .expect("marker");
        assert_eq!(marker.kind, COMPLETION_MARKER);
        assert_eq!(marker.detail, "seq marker");
        let rule = items
            .iter()
            .find(|item| item.label == "expr")
            .expect("rule");
        assert_eq!(rule.kind, COMPLETION_RULE);
        assert_eq!(rule.detail, "rule");
    }

    #[test]
    fn completion_filters_by_word_prefix() {
        let document = Document::open(MINI.to_owned());
        let offset = MINI.find("sum    kind").expect("body") + 1; // inside `sum`
        let items = document.completion_at(offset);
        let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
        assert_eq!(labels, vec!["sum"]);
    }

    // -----------------------------------------------------------------
    // Definition, references, rename
    // -----------------------------------------------------------------

    #[test]
    fn definition_jumps_from_reference_to_declaration() {
        let document = Document::open(MINI.to_owned());
        let ref_offset = MINI.find("sum ").expect("ref") + 1;
        let span = document.definition_at(ref_offset).expect("definition");
        let decl_offset = byte_offset(MINI, "sum    kind=seq");
        assert_eq!(span.start, decl_offset);
        // At the declaration itself there is nowhere to jump.
        assert_eq!(document.definition_at(decl_offset + 1), None);
    }

    #[test]
    fn references_find_all_sites_and_optionally_the_declaration() {
        let document = Document::open(MINI.to_owned());
        let decl_offset = byte_offset(MINI, "expr   kind=alt");
        let sites = document.references_at(decl_offset + 1, false);
        assert_eq!(sites.len(), 1); // `plus expr` inside `sum`
        let sites = document.references_at(decl_offset + 1, true);
        assert_eq!(sites.len(), 2);
        for span in &sites {
            assert_eq!(span.slice(MINI), "expr");
        }
    }

    #[test]
    fn rename_validates_and_covers_every_site() {
        let document = Document::open(RENAME.to_owned());
        let decl_offset = byte_offset(RENAME, "value kind=alt");
        let ref_offset = RENAME.find("seq value").expect("ref") + "seq ".len();

        for offset in [decl_offset + 1, ref_offset + 1] {
            let spans = document.rename_at(offset, "atom").expect("renames");
            assert_eq!(spans.len(), 2);
            for span in &spans {
                assert_eq!(span.slice(RENAME), "value");
            }
            // Spans are ascending (declaration first, then refs).
            assert!(spans[0].start < spans[1].start);
        }

        assert_eq!(
            document.rename_at(decl_offset + 1, ""),
            Err(RenameError::BadName)
        );
        assert_eq!(
            document.rename_at(decl_offset + 1, "9bad"),
            Err(RenameError::BadName)
        );
        assert_eq!(
            document.rename_at(decl_offset + 1, "rep"),
            Err(RenameError::Reserved)
        );
        assert_eq!(
            document.rename_at(decl_offset + 1, "list"),
            Err(RenameError::Duplicate)
        );
        assert_eq!(document.rename_at(0, "x"), Err(RenameError::NoSymbol));
        // Renaming to the same name is a no-op that still succeeds.
        let spans = document
            .rename_at(decl_offset + 1, "value")
            .expect("same name");
        assert_eq!(spans.len(), 2);
    }

    #[test]
    fn document_symbols_cover_tokens_and_rules() {
        let document = Document::open(MINI.to_owned());
        let symbols = document.document_symbols();
        assert_eq!(symbols.len(), 5);
        assert_eq!(symbols[0].name, "number");
        assert_eq!(symbols[0].kind, SYMBOL_TOKEN);
        assert_eq!(symbols[2].name, "expr");
        assert_eq!(symbols[2].kind, SYMBOL_RULE);
        // selectionRange sits inside range.
        for symbol in &symbols {
            assert!(symbol.selection_range.start >= symbol.range.start);
            assert!(symbol.selection_range.end <= symbol.range.end);
        }
    }

    // -----------------------------------------------------------------
    // Incremental edits
    // -----------------------------------------------------------------

    #[test]
    fn ranged_edit_reanalyzes_and_diagnostics_follow() {
        let mut document = Document::open(MINI.to_owned());
        assert!(
            document.diagnostics.is_empty()
                || document.diagnostics[0].severity == DiagSeverity::Hint
        );

        // Break it: rename `sum`'s body ref `expr` → `wat`.
        let ref_offset = MINI.find("plus expr").expect("body") + "plus ".len();
        document.edit(
            Some(ByteSpan {
                start: ref_offset,
                end: ref_offset + 4,
            }),
            "wat",
        );
        assert!(
            document
                .diagnostics
                .iter()
                .any(|diag| diag.severity == DiagSeverity::Error && diag.message.contains("wat"))
        );

        // Fix it again with a full-text edit.
        document.edit(None, MINI);
        assert!(
            !document
                .diagnostics
                .iter()
                .any(|diag| diag.severity == DiagSeverity::Error)
        );
    }

    #[test]
    fn workspace_scopes_features_per_document() {
        let mut workspace = Workspace::new();
        workspace.open("file:///a.gir", MINI.to_owned());
        workspace.open("file:///b.gir", CONFLICT.to_owned());

        // Symbols per file.
        assert_eq!(workspace.document_symbols("file:///a.gir").len(), 5);
        assert_eq!(workspace.document_symbols("file:///b.gir").len(), 6);
        assert_eq!(workspace.document_symbols("file:///missing.gir").len(), 0);

        // Diagnostics per file: `a` has the unreachable hint, `b` the conflict.
        let a = workspace.diagnostics("file:///a.gir");
        assert!(a.iter().any(|diag| diag.message.contains("unreachable")));
        let b = workspace.diagnostics("file:///b.gir");
        assert!(b.iter().any(|diag| diag.message.contains("LL(1) conflict")));

        // Definition and rename work inside either file.
        let b_text = CONFLICT;
        let ref_stmt = b_text.find("alt if-else").expect("ref") + "alt ".len();
        let offset = workspace.offset_of("file:///b.gir", workspace_position(b_text, ref_stmt));
        let definition = workspace
            .definition("file:///b.gir", offset.expect("offset"))
            .expect("def");
        assert_eq!(definition.uri, "file:///b.gir");

        let edit = workspace
            .rename("file:///b.gir", offset.expect("offset"), "xx")
            .expect("renames");
        assert_eq!(edit.changes.len(), 1);
        assert!(edit.changes.contains_key("file:///b.gir"));

        assert!(workspace.close("file:///a.gir"));
        assert!(!workspace.close("file:///a.gir"));
        assert!(workspace.get("file:///a.gir").is_none());
    }

    fn workspace_position(text: &str, offset: usize) -> Position {
        LineIndex::new(text).position(text, offset)
    }
}
