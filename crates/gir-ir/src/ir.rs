//! The canonical typed grammar IR (roadmap §5.1): rules, alternatives,
//! repetitions, predicates and token declarations over interned names and
//! stable dense ids.
//!
//! Layering: KDL (gir-syntax) and importers construct a [`Grammar`] through
//! [`GrammarBuilder`]; every pass (analysis, transforms, codegen) consumes
//! and produces this typed IR — nothing re-parses strings.
//!
//! # Ids
//!
//! [`RuleId`] (u32) and [`TokenId`] (u16) are dense, assigned in declaration
//! order, and stable for the lifetime of the grammar — widths match
//! tree-sitter's proven u16 table widths. Names are interned ([`Sym`]) and
//! resolvable through [`Grammar::interner`].

use std::collections::BTreeSet;
use std::fmt;

use crate::interner::{Interner, Sym};

// ---------------------------------------------------------------------------
// Ids
// ---------------------------------------------------------------------------

/// A grammar rule id (dense, declaration-ordered).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuleId(pub u32);

impl fmt::Display for RuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "rule{}", self.0)
    }
}

/// A token id (dense, declaration-ordered; u16 matches generated tables).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TokenId(pub u16);

impl fmt::Display for TokenId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "tok{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// Elements
// ---------------------------------------------------------------------------

/// One element of a rule body: the grammar-expression tree.
///
/// Every construct is canonical — the surface sugar (e.g. `separated`
/// lists) lowers into these variants before reaching the IR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Element {
    /// Reference to a grammar rule.
    Rule(RuleId),
    /// Reference to a declared token (a leaf).
    Token(TokenId),
    /// Ordered sequence; an empty sequence is the empty match (ε).
    Seq(Vec<Element>),
    /// Ordered choice; the first matching alternative wins.
    Alt(Vec<Element>),
    /// Zero or one occurrence.
    Opt(Box<Element>),
    /// Repetition: at least `min`, at most `max` (`None` = unbounded).
    /// `min == 0 && max.is_none()` is the common `*`; `min == 1` is `+`.
    Rep {
        /// The repeated body.
        inner: Box<Element>,
        /// Minimum occurrences (0 or 1 in canonical grammars).
        min: u32,
        /// Maximum occurrences, `None` = unbounded.
        max: Option<u32>,
    },
    /// Positive lookahead predicate: matches without consuming.
    And(Box<Element>),
    /// Negative lookahead predicate: succeeds when the inner fails,
    /// without consuming.
    Not(Box<Element>),
}

impl Element {
    /// Collect the rule ids referenced anywhere inside this element tree
    /// (deduplicated, ascending).
    #[must_use]
    pub fn rule_refs(&self) -> BTreeSet<RuleId> {
        let mut out = BTreeSet::new();
        self.collect_rule_refs(&mut out);
        out
    }

    fn collect_rule_refs(&self, out: &mut BTreeSet<RuleId>) {
        match self {
            Self::Rule(id) => {
                out.insert(*id);
            }
            Self::Token(_) => {}
            Self::Seq(elements) | Self::Alt(elements) => {
                for element in elements {
                    element.collect_rule_refs(out);
                }
            }
            Self::Opt(inner) | Self::And(inner) | Self::Not(inner) => {
                inner.collect_rule_refs(out);
            }
            Self::Rep { inner, .. } => inner.collect_rule_refs(out),
        }
    }

    /// Collect the token ids referenced anywhere inside this element tree
    /// (deduplicated, ascending).
    #[must_use]
    pub fn token_refs(&self) -> BTreeSet<TokenId> {
        let mut out = BTreeSet::new();
        self.collect_token_refs(&mut out);
        out
    }

    fn collect_token_refs(&self, out: &mut BTreeSet<TokenId>) {
        match self {
            Self::Rule(_) => {}
            Self::Token(id) => {
                out.insert(*id);
            }
            Self::Seq(elements) | Self::Alt(elements) => {
                for element in elements {
                    element.collect_token_refs(out);
                }
            }
            Self::Opt(inner) | Self::And(inner) | Self::Not(inner) => {
                inner.collect_token_refs(out);
            }
            Self::Rep { inner, .. } => inner.collect_token_refs(out),
        }
    }
}

// ---------------------------------------------------------------------------
// Token declarations
// ---------------------------------------------------------------------------

/// What a token matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenPattern {
    /// A fixed literal string (keywords, punctuation).
    Lit {
        /// The literal text.
        text: String,
    },
    /// A regular expression (lexer-compiled).
    Re {
        /// The regex source.
        regex: String,
    },
}

/// A declared token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenDef {
    /// Dense token id.
    pub id: TokenId,
    /// Interned name.
    pub name: Sym,
    /// What the token matches.
    pub pattern: TokenPattern,
    /// Whitespace/comment-class token: never a token event (ADR-008);
    /// rides trivia records instead.
    pub trivia: bool,
    /// Produce a named node for this token in the tree (e.g. string
    /// literals that carry structure).
    pub node: bool,
    /// Structural-index boundary character (issue 032 `@index_bound`):
    /// the scanner treats runs delimited by this byte as opaque regions.
    pub index_bound: Option<String>,
}

// ---------------------------------------------------------------------------
// Annotations (hints, not requirements — §5.1 principle 3)
// ---------------------------------------------------------------------------

/// Associativity of a binary operator (Pratt lowering).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Associativity {
    /// `a + b + c` groups left: `(a + b) + c`.
    Left,
    /// `a = b = c` groups right: `a = (b = c)`.
    Right,
    /// Not associative (comparisons): flat.
    None,
}

/// A Pratt precedence hint on an expression rule (`@pratt`):
/// binding powers and associativity consumed by the descent Pratt
/// backend (issue 023).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PrattHint {
    /// Left binding power.
    pub left_bp: u8,
    /// Right binding power.
    pub right_bp: u8,
    /// Associativity.
    pub assoc: Associativity,
}

// ---------------------------------------------------------------------------
// Rules
// ---------------------------------------------------------------------------

/// A grammar rule: one nonterminal with its canonical body and the
/// annotation set passes consume.
///
/// (The annotation set is flat by design — each flag is an independent
/// pass-facing datum, not a state machine.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// Dense rule id.
    pub id: RuleId,
    /// Interned name.
    pub name: Sym,
    /// Canonical body (`Alt` of alternatives, or a single sequence).
    pub body: Element,
    /// Emit a named tree node for this rule (`@node`).
    pub node: bool,
    /// Worth reusing incrementally (`@reuse`, issue 030).
    pub reuse: bool,
    /// List-shaped rule (separated sugar origin; layout data).
    pub list: bool,
    /// Cold path: error/recovery region, placed `#[cold]` by codegen.
    pub cold: bool,
    /// Speculative-parse boundary (issue 030 `@fragile`).
    pub fragile: bool,
    /// Recovery wraps offending regions of this rule in ERROR nodes.
    pub wrap_error: bool,
    /// Pratt precedence hint (issue 013 lowering; issue 023 backend).
    pub pratt: Option<PrattHint>,
    /// Sync tokens: the parser stops consuming and syncs on these during
    /// recovery (issue 016/026).
    pub sync: Vec<TokenId>,
    /// Missing-token candidates for insertion repairs (issue 016).
    pub insert: Vec<TokenId>,
}

// ---------------------------------------------------------------------------
// Grammar
// ---------------------------------------------------------------------------

/// A canonical typed grammar: tokens + rules + metadata, ids interned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grammar {
    /// Language name (e.g. `"json"`).
    pub name: String,
    /// Language version string (e.g. `"1.0.0"`).
    pub version: String,
    /// File extensions mapped to this grammar (without the dot).
    pub extensions: Vec<String>,
    /// Whether the language treats source as Unicode (affects lexing).
    pub unicode: bool,
    /// Declared tokens, dense by [`TokenId`].
    pub tokens: Vec<TokenDef>,
    /// Declared rules, dense by [`RuleId`].
    pub rules: Vec<Rule>,
    /// The start rule.
    pub start: RuleId,
    /// The name interner backing every [`Sym`] in this grammar.
    pub interner: Interner,
}

impl Grammar {
    /// The rule for `id`, or `None` when out of bounds (hand-built
    /// grammars can carry stale ids; [`Grammar::validate`] catches them).
    #[must_use]
    pub fn try_rule(&self, id: RuleId) -> Option<&Rule> {
        self.rules.get(usize::try_from(id.0).ok()?)
    }

    /// The rule for `id` (panics on an out-of-bounds id — grammars built
    /// through [`GrammarBuilder`] guarantee validity).
    ///
    /// # Panics
    /// When `id` does not index [`Self::rules`].
    #[must_use]
    pub fn rule(&self, id: RuleId) -> &Rule {
        &self.rules[usize::try_from(id.0).expect("rule id in bounds")]
    }

    /// The token for `id`, or `None` when out of bounds.
    #[must_use]
    pub fn try_token(&self, id: TokenId) -> Option<&TokenDef> {
        self.tokens.get(usize::from(id.0))
    }

    /// The token for `id` (panics on an out-of-bounds id).
    ///
    /// # Panics
    /// When `id` does not index [`Self::tokens`].
    #[must_use]
    pub fn token(&self, id: TokenId) -> &TokenDef {
        &self.tokens[usize::from(id.0)]
    }

    /// The name of a rule (interner-backed).
    #[must_use]
    pub fn rule_name(&self, id: RuleId) -> &str {
        self.interner.lookup(self.rule(id).name)
    }

    /// The name of a token (interner-backed).
    #[must_use]
    pub fn token_name(&self, id: TokenId) -> &str {
        self.interner.lookup(self.token(id).name)
    }

    /// Validate structural invariants:
    ///
    /// - at least one rule, and `start` in bounds;
    /// - every element reference in bounds (hand-built grammars);
    /// - no empty `Alt` (a rule with zero alternatives);
    /// - annotation token ids (`sync`/`insert`) in bounds.
    ///
    /// # Errors
    /// A descriptive [`IrError`] naming the offending rule.
    ///
    /// # Panics
    /// When the rule count exceeds `u32::MAX` or the token count
    /// exceeds `u16::MAX` (index-space exhaustion, not user input).
    pub fn validate(&self) -> Result<(), IrError> {
        if self.rules.is_empty() {
            return Err(IrError::EmptyGrammar);
        }
        let rule_len = u32::try_from(self.rules.len()).expect("rule count fits u32");
        let token_len = u16::try_from(self.tokens.len()).expect("token count fits u16");
        if self.start.0 >= rule_len {
            return Err(IrError::StartOutOfBounds {
                start: self.start,
                rules: self.rules.len(),
            });
        }
        for rule in &self.rules {
            self.check_element(&rule.body, rule.id)?;
            for id in &rule.sync {
                if id.0 >= token_len {
                    return Err(IrError::TokenOutOfBounds {
                        rule: rule.id,
                        token: *id,
                    });
                }
            }
            for id in &rule.insert {
                if id.0 >= token_len {
                    return Err(IrError::TokenOutOfBounds {
                        rule: rule.id,
                        token: *id,
                    });
                }
            }
        }
        Ok(())
    }

    fn check_element(&self, element: &Element, rule: RuleId) -> Result<(), IrError> {
        let rule_len = u32::try_from(self.rules.len()).expect("rule count fits u32");
        let token_len = u16::try_from(self.tokens.len()).expect("token count fits u16");
        match element {
            Element::Rule(id) => {
                if id.0 >= rule_len {
                    return Err(IrError::RuleOutOfBounds { rule, target: *id });
                }
            }
            Element::Token(id) => {
                if id.0 >= token_len {
                    return Err(IrError::TokenOutOfBounds { rule, token: *id });
                }
            }
            Element::Alt(elements) => {
                if elements.is_empty() {
                    return Err(IrError::EmptyAlternatives { rule });
                }
                for element in elements {
                    self.check_element(element, rule)?;
                }
            }
            Element::Seq(elements) => {
                for element in elements {
                    self.check_element(element, rule)?;
                }
            }
            Element::Opt(inner) | Element::And(inner) | Element::Not(inner) => {
                self.check_element(inner, rule)?;
            }
            Element::Rep { inner, min, max } => {
                if let Some(max) = max {
                    if max < min {
                        return Err(IrError::RepetitionBounds {
                            rule,
                            min: *min,
                            max: *max,
                        });
                    }
                }
                self.check_element(inner, rule)?;
            }
        }
        Ok(())
    }

    /// Rules not reachable from the start rule (analysis input; codegen
    /// skips them with a warning).
    ///
    /// # Panics
    /// When the rule count exceeds `u32::MAX` (index-space exhaustion).
    #[must_use]
    pub fn unreachable_rules(&self) -> Vec<RuleId> {
        let mut reachable = BTreeSet::new();
        let mut work = vec![self.start];
        while let Some(id) = work.pop() {
            if !reachable.insert(id) {
                continue;
            }
            let Some(rule) = self.try_rule(id) else {
                continue;
            };
            for target in rule.body.rule_refs() {
                work.push(target);
            }
        }
        (0..self.rules.len())
            .map(|i| RuleId(u32::try_from(i).expect("rule index fits u32")))
            .filter(|id| !reachable.contains(id))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// A grammar-IR construction or validation error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrError {
    /// Two rules share a name.
    DuplicateRule {
        /// The duplicated name.
        name: String,
    },
    /// Two tokens share a name.
    DuplicateToken {
        /// The duplicated name.
        name: String,
    },
    /// A rule body has zero alternatives.
    EmptyAlternatives {
        /// The offending rule.
        rule: RuleId,
    },
    /// A repetition's max is below its min.
    RepetitionBounds {
        /// The offending rule.
        rule: RuleId,
        /// The minimum.
        min: u32,
        /// The maximum (below min).
        max: u32,
    },
    /// The start rule does not index the rule table.
    StartOutOfBounds {
        /// The bad start id.
        start: RuleId,
        /// The rule-table length.
        rules: usize,
    },
    /// An element references a rule past the table.
    RuleOutOfBounds {
        /// The referencing rule.
        rule: RuleId,
        /// The bad target id.
        target: RuleId,
    },
    /// An element (or annotation) references a token past the table.
    TokenOutOfBounds {
        /// The referencing rule.
        rule: RuleId,
        /// The bad token id.
        token: TokenId,
    },
    /// The grammar declares no rules.
    EmptyGrammar,
}

impl fmt::Display for IrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateRule { name } => write!(f, "duplicate rule name `{name}`"),
            Self::DuplicateToken { name } => write!(f, "duplicate token name `{name}`"),
            Self::EmptyAlternatives { rule } => {
                write!(f, "rule {rule} has no alternatives")
            }
            Self::RepetitionBounds { rule, min, max } => {
                write!(f, "rule {rule}: repetition max {max} below min {min}")
            }
            Self::StartOutOfBounds { start, rules } => {
                write!(f, "start rule {start} out of bounds ({rules} rules)")
            }
            Self::RuleOutOfBounds { rule, target } => {
                write!(f, "rule {rule} references out-of-bounds {target}")
            }
            Self::TokenOutOfBounds { rule, token } => {
                write!(f, "rule {rule} references out-of-bounds {token}")
            }
            Self::EmptyGrammar => write!(f, "grammar declares no rules"),
        }
    }
}

impl std::error::Error for IrError {}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Builder for a [`Grammar`]: assigns dense ids at declaration time,
/// interns names, and validates on finish.
///
/// Bodies reference rules by [`RuleId`]; [`Self::declare_rule`] returns
/// the id immediately, so forward references are declared first and their
/// bodies filled via [`Self::set_body`].
#[derive(Debug, Default)]
pub struct GrammarBuilder {
    name: String,
    version: String,
    extensions: Vec<String>,
    unicode: bool,
    tokens: Vec<TokenDef>,
    token_ids: std::collections::HashMap<String, TokenId>,
    rules: Vec<Rule>,
    rule_ids: std::collections::HashMap<String, RuleId>,
    interner: Interner,
}

impl GrammarBuilder {
    /// A builder for a language named `name`.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            ..Self::default()
        }
    }

    /// Set the language version string.
    pub fn version(&mut self, version: &str) -> &mut Self {
        version.clone_into(&mut self.version);
        self
    }

    /// Add a file extension (without the dot).
    pub fn extension(&mut self, extension: &str) -> &mut Self {
        self.extensions.push(extension.to_owned());
        self
    }

    /// Mark the language as Unicode-aware.
    pub fn unicode(&mut self) -> &mut Self {
        self.unicode = true;
        self
    }

    /// Declare a literal token; returns its dense [`TokenId`].
    ///
    /// # Errors
    /// [`IrError::DuplicateToken`] when `name` was already declared.
    pub fn token_lit(&mut self, name: &str, text: &str) -> Result<TokenId, IrError> {
        let id = self.next_token_id(name)?;
        self.tokens.push(TokenDef {
            id,
            name: self.interner.intern(name),
            pattern: TokenPattern::Lit {
                text: text.to_owned(),
            },
            trivia: false,
            node: false,
            index_bound: None,
        });
        Ok(id)
    }

    /// Declare a regex token; returns its dense [`TokenId`].
    ///
    /// # Errors
    /// [`IrError::DuplicateToken`] when `name` was already declared.
    pub fn token_re(&mut self, name: &str, regex: &str) -> Result<TokenId, IrError> {
        let id = self.next_token_id(name)?;
        self.tokens.push(TokenDef {
            id,
            name: self.interner.intern(name),
            pattern: TokenPattern::Re {
                regex: regex.to_owned(),
            },
            trivia: false,
            node: false,
            index_bound: None,
        });
        Ok(id)
    }

    fn next_token_id(&mut self, name: &str) -> Result<TokenId, IrError> {
        if self.token_ids.contains_key(name) {
            return Err(IrError::DuplicateToken {
                name: name.to_owned(),
            });
        }
        let id = TokenId(u16::try_from(self.tokens.len()).expect("token count fits u16"));
        self.token_ids.insert(name.to_owned(), id);
        Ok(id)
    }

    /// Flip the `trivia` flag on a token (whitespace/comments).
    pub fn set_trivia(&mut self, id: TokenId, trivia: bool) -> &mut Self {
        self.tokens[usize::from(id.0)].trivia = trivia;
        self
    }

    /// Flip the `node` flag on a token (tree-visible leaf).
    pub fn set_node(&mut self, id: TokenId, node: bool) -> &mut Self {
        self.tokens[usize::from(id.0)].node = node;
        self
    }

    /// Set the structural-index boundary string on a token.
    pub fn set_index_bound(&mut self, id: TokenId, bound: &str) -> &mut Self {
        self.tokens[usize::from(id.0)].index_bound = Some(bound.to_owned());
        self
    }

    /// Declare a rule (body filled later via [`Self::set_body`]); returns
    /// its dense [`RuleId`]. Forward references work: declare first,
    /// reference the id anywhere, then set bodies.
    ///
    /// # Errors
    /// [`IrError::DuplicateRule`] when `name` was already declared.
    ///
    /// # Panics
    /// When the rule count exceeds `u32::MAX` or the interner exhausts
    /// `u32::MAX` symbols.
    pub fn declare_rule(&mut self, name: &str) -> Result<RuleId, IrError> {
        if self.rule_ids.contains_key(name) {
            return Err(IrError::DuplicateRule {
                name: name.to_owned(),
            });
        }
        let id = RuleId(u32::try_from(self.rules.len()).expect("rule count fits u32"));
        self.rule_ids.insert(name.to_owned(), id);
        self.rules.push(Rule {
            id,
            name: self.interner.intern(name),
            body: Element::Alt(Vec::new()),
            node: false,
            reuse: false,
            list: false,
            cold: false,
            fragile: false,
            wrap_error: false,
            pratt: None,
            sync: Vec::new(),
            insert: Vec::new(),
        });
        Ok(id)
    }

    /// Set (or replace) a rule's canonical body.
    ///
    /// # Panics
    /// When `id` is not a rule previously returned by
    /// [`Self::declare_rule`] (out-of-bounds).
    pub fn set_body(&mut self, id: RuleId, body: Element) -> &mut Self {
        self.rules[usize::try_from(id.0).expect("rule id in bounds")].body = body;
        self
    }

    /// Look up a rule id by name (references by name from surfaces).
    #[must_use]
    pub fn rule_id(&self, name: &str) -> Option<RuleId> {
        self.rule_ids.get(name).copied()
    }

    /// Look up a token id by name.
    #[must_use]
    pub fn token_id(&self, name: &str) -> Option<TokenId> {
        self.token_ids.get(name).copied()
    }

    /// Set the `node` annotation (`@node`) on a rule.
    ///
    /// # Panics
    /// When `id` is out of bounds (not a declared rule).
    pub fn set_node_rule(&mut self, id: RuleId, node: bool) -> &mut Self {
        self.rules[usize::try_from(id.0).expect("rule id in bounds")].node = node;
        self
    }

    /// Set the `reuse` annotation (`@reuse`) on a rule.
    ///
    /// # Panics
    /// When `id` is out of bounds (not a declared rule).
    pub fn set_reuse(&mut self, id: RuleId, reuse: bool) -> &mut Self {
        self.rules[usize::try_from(id.0).expect("rule id in bounds")].reuse = reuse;
        self
    }

    /// Set the `list` annotation (separated-sugar origin) on a rule.
    ///
    /// # Panics
    /// When `id` is out of bounds (not a declared rule).
    pub fn set_list(&mut self, id: RuleId, list: bool) -> &mut Self {
        self.rules[usize::try_from(id.0).expect("rule id in bounds")].list = list;
        self
    }

    /// Set the `wrap_error` recovery annotation on a rule.
    ///
    /// # Panics
    /// When `id` is out of bounds (not a declared rule).
    pub fn set_wrap_error(&mut self, id: RuleId, wrap_error: bool) -> &mut Self {
        self.rules[usize::try_from(id.0).expect("rule id in bounds")].wrap_error = wrap_error;
        self
    }

    /// Set the Pratt hint on a rule.
    ///
    /// # Panics
    /// When `id` is out of bounds (not a declared rule).
    pub fn set_pratt(&mut self, id: RuleId, hint: PrattHint) -> &mut Self {
        self.rules[usize::try_from(id.0).expect("rule id in bounds")].pratt = Some(hint);
        self
    }

    /// Append sync tokens to a rule's recovery sync set.
    ///
    /// # Panics
    /// When `id` is out of bounds (not a declared rule).
    pub fn add_sync(&mut self, id: RuleId, tokens: &[TokenId]) -> &mut Self {
        self.rules[usize::try_from(id.0).expect("rule id in bounds")]
            .sync
            .extend_from_slice(tokens);
        self
    }

    /// Set the missing-token insertion candidates on a rule.
    ///
    /// # Panics
    /// When `id` is out of bounds (not a declared rule).
    pub fn set_insert(&mut self, id: RuleId, tokens: Vec<TokenId>) -> &mut Self {
        self.rules[usize::try_from(id.0).expect("rule id in bounds")].insert = tokens;
        self
    }

    /// Ordered sequence element.
    #[must_use]
    pub fn seq(elements: Vec<Element>) -> Element {
        Element::Seq(elements)
    }

    /// Ordered choice element.
    #[must_use]
    pub fn alt(elements: Vec<Element>) -> Element {
        Element::Alt(elements)
    }

    /// `element?`
    #[must_use]
    pub fn opt(element: Element) -> Element {
        Element::Opt(Box::new(element))
    }

    /// `element*`
    #[must_use]
    pub fn rep0(element: Element) -> Element {
        Self::rep(element, 0, None)
    }

    /// `element+`
    #[must_use]
    pub fn rep1(element: Element) -> Element {
        Self::rep(element, 1, None)
    }

    /// `element{min,max}` (`max = None` unbounded).
    #[must_use]
    pub fn rep(element: Element, min: u32, max: Option<u32>) -> Element {
        Element::Rep {
            inner: Box::new(element),
            min,
            max,
        }
    }

    /// `&element` positive lookahead.
    #[must_use]
    pub fn and(element: Element) -> Element {
        Element::And(Box::new(element))
    }

    /// `!element` negative lookahead.
    #[must_use]
    pub fn not(element: Element) -> Element {
        Element::Not(Box::new(element))
    }

    /// Reference a rule as an element.
    #[must_use]
    pub fn rule_ref(id: RuleId) -> Element {
        Element::Rule(id)
    }

    /// Reference a token as an element.
    #[must_use]
    pub fn token_ref(id: TokenId) -> Element {
        Element::Token(id)
    }

    /// Finish: default the start rule to the first declared rule,
    /// validate, and produce the canonical [`Grammar`].
    ///
    /// # Errors
    /// Anything [`Grammar::validate`] reports (plus the empty-grammar
    /// case when no rule was declared).
    pub fn finish(self) -> Result<Grammar, IrError> {
        let start = self
            .rules
            .first()
            .map(|rule| rule.id)
            .ok_or(IrError::EmptyGrammar)?;
        let grammar = Grammar {
            name: self.name,
            version: self.version,
            extensions: self.extensions,
            unicode: self.unicode,
            tokens: self.tokens,
            rules: self.rules,
            start,
            interner: self.interner,
        };
        grammar.validate()?;
        Ok(grammar)
    }
}
