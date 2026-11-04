//! Production lowering for the shadow automaton (issue 011): GIR element
//! trees → flat `A → α` productions over an alphabet of tokens,
//! nonterminals and the pseudo-EOF sentinel.
//!
//! Canonical LR(1) construction consumes flat productions, not the GIR's
//! expression tree. The lowering is mechanical: `Alt` forks productions of
//! the same left-hand side, `Opt`/`Rep` expand into synthetic nullable
//! nonterminals, and predicates (GIR `And`/`Not`) become ε-producing
//! placeholders whose element is preserved in [`Lowered::predicates`] for
//! later passes (codegen, recovery planning).

use std::collections::{BTreeMap, BTreeSet};

use gir_ir::{Element, Grammar, RuleId, TokenId};

// ---------------------------------------------------------------------------
// Alphabet
// ---------------------------------------------------------------------------

/// A lookahead terminal: a token id or the pseudo-EOF sentinel.
///
/// The sentinel lives at [`Look::EOF`] (`u32::MAX`), outside the `u16`
/// token-id space — extending the grammar's token table can never
/// reinterpret it (issue 011 "pseudo-EOF resize safety").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Look(pub u32);

impl Look {
    /// The pseudo-EOF sentinel (`u32::MAX`; a [`TokenId`] is a `u16` and
    /// can therefore never collide with it).
    pub const EOF: Self = Self(u32::MAX);

    /// Wrap a token id.
    #[must_use]
    pub const fn token(id: TokenId) -> Self {
        Self(id.0 as u32)
    }

    /// Whether this is the pseudo-EOF sentinel.
    #[must_use]
    pub const fn is_eof(self) -> bool {
        self.0 == Self::EOF.0
    }

    /// The underlying token id, or `None` for EOF.
    #[must_use]
    pub fn as_token(self) -> Option<TokenId> {
        u16::try_from(self.0).ok().map(TokenId)
    }
}

impl std::fmt::Display for Look {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_eof() {
            write!(f, "eof")
        } else {
            write!(f, "tok{}", self.0)
        }
    }
}

/// A nonterminal id: original rules keep their [`RuleId`] numbering,
/// synthetic helpers continue after them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NontermId(pub u32);

impl std::fmt::Display for NontermId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "nt{}", self.0)
    }
}

/// One symbol of a production right-hand side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Symbol {
    /// Terminal token.
    Token(TokenId),
    /// Nonterminal reference.
    Nonterm(NontermId),
    /// The pseudo-EOF marker (only the augmented start production
    /// contains it).
    Eof,
}

/// Why a production exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProdKind {
    /// Direct lowering of rule body content.
    Normal,
    /// Synthetic empty alternative (`Opt`/`Rep` expansion or empty `Seq`).
    Epsilon,
    /// Lookahead predicate placeholder (GIR `And`/`Not`): matches ε.
    Predicate,
}

/// One `lhs → symbols` production.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Production {
    /// The left-hand side nonterminal.
    pub lhs: NontermId,
    /// The right-hand side symbols.
    pub symbols: Vec<Symbol>,
    /// Why this production exists.
    pub kind: ProdKind,
    /// The GIR rule this production lowers (`None` for synthetics).
    pub origin: Option<RuleId>,
}

/// A preserved lookahead predicate (GIR `And`/`Not`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Predicate {
    /// `true` for `And` (must match), `false` for `Not` (must fail).
    pub positive: bool,
    /// The predicate's inner element.
    pub element: Element,
}

// ---------------------------------------------------------------------------
// Result
// ---------------------------------------------------------------------------

/// The lowered form of a [`Grammar`]: flat productions over the token /
/// nonterminal / EOF alphabet, ready for LR(1) construction.
#[derive(Debug, Clone)]
pub struct Lowered {
    /// All productions; index is the production id used by items.
    pub productions: Vec<Production>,
    /// Number of nonterminals: original rules first (same numbering as
    /// [`RuleId`]), then synthetics, then the augmented start.
    pub nonterms: u32,
    /// The start nonterminal (the grammar's start rule).
    pub start: NontermId,
    /// Preserved predicates, keyed by their placeholder nonterminal.
    pub predicates: BTreeMap<NontermId, Predicate>,
}

impl Lowered {
    /// Productions with the given left-hand side.
    ///
    /// # Panics
    /// When the production count exceeds `u32::MAX`
    /// (index-space exhaustion).
    #[must_use]
    pub fn productions_of(&self, lhs: NontermId) -> Vec<u32> {
        self.productions
            .iter()
            .enumerate()
            .filter(|(_, production)| production.lhs == lhs)
            .map(|(idx, _)| u32::try_from(idx).expect("production count fits u32"))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Lowering
// ---------------------------------------------------------------------------

/// Lower a grammar into productions.
///
/// # Panics
/// When the grammar exceeds `u32::MAX` rules (index-space exhaustion).
#[must_use]
pub fn lower(grammar: &Grammar) -> Lowered {
    let mut ctx = Ctx {
        productions: Vec::new(),
        predicates: BTreeMap::new(),
        nonterms: u32::try_from(grammar.rules.len()).expect("rule count fits u32"),
    };
    for rule in &grammar.rules {
        let lhs = NontermId(rule.id.0);
        ctx.lower_body(lhs, &rule.body, Some(rule.id), ProdKind::Normal);
    }
    Lowered {
        productions: ctx.productions,
        nonterms: ctx.nonterms,
        start: NontermId(grammar.start.0),
        predicates: ctx.predicates,
    }
}

struct Ctx {
    productions: Vec<Production>,
    predicates: BTreeMap<NontermId, Predicate>,
    nonterms: u32,
}

impl Ctx {
    /// Allocate a fresh synthetic nonterminal.
    fn synthetic(&mut self) -> NontermId {
        let id = self.nonterms;
        self.nonterms += 1;
        NontermId(id)
    }

    fn push(&mut self, production: Production) {
        self.productions.push(production);
    }

    /// Lower `body` as the full body of `lhs` (one production per
    /// alternative; predicates and empty matches keep their kind).
    fn lower_body(
        &mut self,
        lhs: NontermId,
        body: &Element,
        origin: Option<RuleId>,
        kind: ProdKind,
    ) {
        match body {
            Element::Alt(branches) => {
                for branch in branches {
                    self.lower_body(lhs, branch, origin, kind);
                }
            }
            Element::Seq(elements) => {
                let symbols: Vec<Symbol> = elements.iter().map(|e| self.lower_element(e)).collect();
                let kind = if symbols.is_empty() && kind == ProdKind::Normal {
                    ProdKind::Epsilon
                } else {
                    kind
                };
                self.push(Production {
                    lhs,
                    symbols,
                    kind,
                    origin,
                });
            }
            Element::Opt(inner) => {
                let symbols = vec![self.lower_element(inner)];
                self.push(Production {
                    lhs,
                    symbols,
                    kind,
                    origin,
                });
                self.push(Production {
                    lhs,
                    symbols: Vec::new(),
                    kind: ProdKind::Epsilon,
                    origin,
                });
            }
            Element::Rep { inner, min, max } => self.lower_rep(lhs, inner, *min, *max, origin),
            Element::And(inner) => {
                let placeholder = self.synthetic();
                self.predicates.insert(
                    placeholder,
                    Predicate {
                        positive: true,
                        element: (**inner).clone(),
                    },
                );
                self.push(Production {
                    lhs: placeholder,
                    symbols: Vec::new(),
                    kind: ProdKind::Predicate,
                    origin,
                });
                self.push(Production {
                    lhs,
                    symbols: vec![Symbol::Nonterm(placeholder)],
                    kind,
                    origin,
                });
            }
            Element::Not(inner) => {
                let placeholder = self.synthetic();
                self.predicates.insert(
                    placeholder,
                    Predicate {
                        positive: false,
                        element: (**inner).clone(),
                    },
                );
                self.push(Production {
                    lhs: placeholder,
                    symbols: Vec::new(),
                    kind: ProdKind::Predicate,
                    origin,
                });
                self.push(Production {
                    lhs,
                    symbols: vec![Symbol::Nonterm(placeholder)],
                    kind,
                    origin,
                });
            }
            // A bare token or rule reference: a one-symbol production.
            Element::Token(_) | Element::Rule(_) => {
                let symbols = vec![self.lower_element(body)];
                self.push(Production {
                    lhs,
                    symbols,
                    kind,
                    origin,
                });
            }
        }
    }

    /// Lower `Rep`: unroll the minimum, then a star (unbounded) or
    /// `max - min` optionals (bounded).
    fn lower_rep(
        &mut self,
        lhs: NontermId,
        inner: &Element,
        min: u32,
        max: Option<u32>,
        origin: Option<RuleId>,
    ) {
        let inner_sym = self.lower_element(inner);
        let mut symbols = vec![inner_sym; min as usize];
        match max {
            None => {
                // tail: star → ε | inner star
                let star = self.synthetic();
                self.push(Production {
                    lhs: star,
                    symbols: Vec::new(),
                    kind: ProdKind::Epsilon,
                    origin,
                });
                self.push(Production {
                    lhs: star,
                    symbols: vec![inner_sym, Symbol::Nonterm(star)],
                    kind: ProdKind::Normal,
                    origin,
                });
                symbols.push(Symbol::Nonterm(star));
            }
            Some(max) => {
                let optionals = max.saturating_sub(min);
                if optionals > 0 {
                    // Shared optional nonterminal: syn → inner | ε.
                    let opt = self.synthetic();
                    self.push(Production {
                        lhs: opt,
                        symbols: vec![inner_sym],
                        kind: ProdKind::Normal,
                        origin,
                    });
                    self.push(Production {
                        lhs: opt,
                        symbols: Vec::new(),
                        kind: ProdKind::Epsilon,
                        origin,
                    });
                    symbols.extend(std::iter::repeat_n(
                        Symbol::Nonterm(opt),
                        optionals as usize,
                    ));
                }
            }
        }
        let kind = if symbols.is_empty() {
            ProdKind::Epsilon
        } else {
            ProdKind::Normal
        };
        self.push(Production {
            lhs,
            symbols,
            kind,
            origin,
        });
    }

    /// Lower one element **position**: everything that is not a single
    /// symbol gets a synthetic nonterminal so grouping is preserved
    /// (an `Alt` inside a `Seq` must not fork the whole sequence).
    fn lower_element(&mut self, element: &Element) -> Symbol {
        match element {
            Element::Token(id) => Symbol::Token(*id),
            Element::Rule(id) => Symbol::Nonterm(NontermId(id.0)),
            Element::Seq(elements) if elements.len() == 1 => self.lower_element(&elements[0]),
            other => {
                let syn = self.synthetic();
                self.lower_body(syn, other, None, ProdKind::Normal);
                Symbol::Nonterm(syn)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// FIRST / nullable over the lowered grammar
// ---------------------------------------------------------------------------

/// Nullable flags + FIRST token sets per nonterminal of a [`Lowered`].
///
/// Both are monotone over finite lattices; the fixpoint terminates.
#[derive(Debug, Clone)]
pub struct First {
    /// Nullable per nonterminal id.
    pub nullable: Vec<bool>,
    /// FIRST terminal set per nonterminal id.
    pub first: Vec<BTreeSet<Look>>,
}

impl First {
    /// Compute the fixpoint over `lowered`.
    #[must_use]
    pub fn compute(lowered: &Lowered) -> Self {
        let n = lowered.nonterms as usize;
        let mut nullable = vec![false; n];
        let mut first: Vec<BTreeSet<Look>> = vec![BTreeSet::new(); n];
        loop {
            let mut changed = false;
            for production in &lowered.productions {
                let idx = production.lhs.0 as usize;
                // FIRST(lhs) ⊇ FIRST(prefix of rhs until non-nullable);
                // a right-hand side that is entirely nullable makes the
                // left-hand side nullable (ε-productions are the empty
                // case of the same rule).
                let mut all_nullable = true;
                for symbol in &production.symbols {
                    match symbol {
                        Symbol::Token(id) => {
                            changed |= first[idx].insert(Look::token(*id));
                            all_nullable = false;
                            break;
                        }
                        Symbol::Eof => {
                            changed |= first[idx].insert(Look::EOF);
                            all_nullable = false;
                            break;
                        }
                        Symbol::Nonterm(nt) => {
                            let nt_idx = nt.0 as usize;
                            let contributed: Vec<Look> = first[nt_idx].iter().copied().collect();
                            let before = first[idx].len();
                            first[idx].extend(contributed);
                            changed |= first[idx].len() != before;
                            if !nullable[nt_idx] {
                                all_nullable = false;
                                break;
                            }
                        }
                    }
                }
                if all_nullable && !nullable[idx] {
                    nullable[idx] = true;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        Self { nullable, first }
    }
}

/// FIRST of a production suffix, seeded with the item lookahead when the
/// whole suffix is nullable (the LR(1) closure rule).
#[must_use]
pub fn first_of_suffix(suffix: &[Symbol], la: Look, first: &First) -> BTreeSet<Look> {
    let mut out = BTreeSet::new();
    for symbol in suffix {
        match symbol {
            Symbol::Token(id) => {
                out.insert(Look::token(*id));
                return out;
            }
            Symbol::Eof => {
                out.insert(Look::EOF);
                return out;
            }
            Symbol::Nonterm(nt) => {
                let idx = nt.0 as usize;
                out.extend(first.first[idx].iter().copied());
                if !first.nullable[idx] {
                    return out;
                }
            }
        }
    }
    out.insert(la);
    out
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use gir_ir::{Grammar, GrammarBuilder, RuleId, TokenId};

    use super::{First, Look, NontermId, ProdKind, Symbol, first_of_suffix, lower};

    /// A one-rule grammar; `set` fills the body of rule `s` given token
    /// `a`. Returns the grammar and `a`'s id.
    fn tiny_grammar(set: fn(&mut GrammarBuilder, TokenId, RuleId)) -> (Grammar, TokenId) {
        let mut b = GrammarBuilder::new("tiny");
        let a = b.token_lit("a", "a").expect("t");
        let s = b.declare_rule("s").expect("r");
        set(&mut b, a, s);
        (b.finish().expect("valid"), a)
    }

    #[test]
    fn look_orders_tokens_before_eof_and_round_trips() {
        assert!(Look::token(TokenId(0)) < Look::EOF);
        assert!(Look::token(TokenId(65_535)) < Look::EOF);
        assert_eq!(Look::token(TokenId(9)).as_token(), Some(TokenId(9)));
        assert!(Look::EOF.is_eof());
    }

    #[test]
    fn opt_expands_to_symbol_plus_epsilon() {
        let (grammar, _a) = tiny_grammar(|b, a, s| {
            b.set_body(s, GrammarBuilder::opt(GrammarBuilder::token_ref(a)));
        });
        let lowered = lower(&grammar);
        // Body-level Opt lowers inline: s → [a] | ε (no synthetic).
        assert_eq!(lowered.productions.len(), 2);
        let epsilons = lowered
            .productions
            .iter()
            .filter(|p| p.kind == ProdKind::Epsilon)
            .count();
        assert_eq!(epsilons, 1);
    }

    #[test]
    fn rep0_expands_to_star_loop() {
        let (grammar, a) = tiny_grammar(|b, a, s| {
            b.set_body(s, GrammarBuilder::rep0(GrammarBuilder::token_ref(a)));
        });
        let lowered = lower(&grammar);
        // s → [star]; star → ε | [a, star] (one synthetic for the star).
        assert_eq!(lowered.productions.len(), 3);
        let star = lowered
            .productions
            .iter()
            .find(|p| p.symbols.len() == 2)
            .expect("star loop production");
        assert_eq!(star.symbols[0], Symbol::Token(a));
        assert_eq!(star.symbols[1], Symbol::Nonterm(star.lhs), "self-recursive");
    }

    #[test]
    fn predicates_are_preserved_with_polarity() {
        let (grammar, _a) = tiny_grammar(|b, a, s| {
            b.set_body(
                s,
                GrammarBuilder::seq(vec![
                    GrammarBuilder::and(GrammarBuilder::token_ref(a)),
                    GrammarBuilder::not(GrammarBuilder::token_ref(a)),
                    GrammarBuilder::token_ref(a),
                ]),
            );
        });
        let lowered = lower(&grammar);
        assert_eq!(lowered.predicates.len(), 2);
        let polarities: Vec<bool> = lowered.predicates.values().map(|p| p.positive).collect();
        assert!(polarities.contains(&true));
        assert!(polarities.contains(&false));
    }

    #[test]
    fn nullable_propagates_through_all_nullable_rhs() {
        // Regression: s → t; t → a? — both must end up nullable even
        // though only the synthetic has an ε production.
        let (grammar, _a) = tiny_grammar(|b, a, s| {
            let t = b.declare_rule("t").expect("r");
            b.set_body(t, GrammarBuilder::opt(GrammarBuilder::token_ref(a)));
            b.set_body(s, GrammarBuilder::rule_ref(t));
        });
        let lowered = lower(&grammar);
        let first = First::compute(&lowered);
        assert!(first.nullable[lowered.start.0 as usize], "s is nullable");
    }

    #[test]
    fn first_of_suffix_seeds_lookahead_when_nullable() {
        let (grammar, a) = tiny_grammar(|b, a, s| {
            let t = b.declare_rule("t").expect("r");
            b.set_body(t, GrammarBuilder::opt(GrammarBuilder::token_ref(a)));
            b.set_body(
                s,
                GrammarBuilder::seq(vec![
                    GrammarBuilder::rule_ref(t),
                    GrammarBuilder::token_ref(a),
                ]),
            );
        });
        let lowered = lower(&grammar);
        let first = First::compute(&lowered);
        // Suffix [t, a]: FIRST = {a} (t is nullable but `a` follows).
        let suffix = [Symbol::Nonterm(NontermId(1)), Symbol::Token(a)];
        let las = first_of_suffix(&suffix, Look::EOF, &first);
        assert_eq!(las, BTreeSet::from([Look::token(a)]));
        // Suffix [t] alone: nullable → seeded with the item lookahead.
        let las = first_of_suffix(&[Symbol::Nonterm(NontermId(1))], Look::EOF, &first);
        assert_eq!(las, BTreeSet::from([Look::token(a), Look::EOF]));
    }
}
