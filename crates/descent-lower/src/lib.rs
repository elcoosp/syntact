//! # descent-lower — the grammar → descriptor compiler (issue 043, §7.4)
//!
//! Closes the compiler loop: the front half (`gir-syntax` → `gir-ir` →
//! `gir-analysis`) has always *analyzed* grammars, and the runtime
//! (`descent`) has always *interpreted* descriptors — but nothing
//! connected the two, so every pack parser was hand-written. This crate
//! is the keystone: a deterministic lowering from the canonical typed IR
//! into the [`descent::Desc`] descriptor shape the descent engine (and
//! the codegen table backend) consumes — "the interpreter and the
//! generated parser must agree by construction" stops being an
//! aspiration and becomes a compiled fact.
//!
//! ## What it lowers
//!
//! - **Kind space** — one dense `u16` map ([`Lowered`]): `node="true"`
//!   rules in declaration order, then `node="true"` tokens, then plain
//!   tokens, then trivia; the unmatched-character sentinel
//!   ([`Lowered::bad_kind`]) lands last. Pratt operator nodes reuse the
//!   operator token's kind (tree-sitter's anonymous-node convention),
//!   so `a + b` is a node of the `+` kind wrapping its operands.
//! - **Transparent rules** — `node="false"` rules inline at their call
//!   sites (the engine's `Pat::Rule` always emits a node). The
//!   transparent subgraph must be acyclic; a cycle is a named error.
//! - **LL(1) dispatch** — `Alt` branches dispatch through
//!   `Pat::Peek`/`Pat::PeekSet` guards computed from FIRST sets
//!   (`gir-analysis`); overlapping FIRSTs are an
//!   [`LowerError::AltConflict`], not a silent guess. Rule-headed
//!   optional and repetition bodies get FIRST-guards so valid input
//!   never speculatively enters a self-recovering rule (the `{}` case).
//! - **Sync sets** — the declared `sync` tokens when present, else the
//!   rule's FOLLOW set: recovery stops at exactly the tokens that may
//!   legally follow the rule.
//! - **Pratt rules** — a hint-carrying rule (`lbp=`) lowers to one
//!   [`descent::PrattDesc`] table: leading-self branches become infix
//!   entries, bare-operator tails become postfix entries,
//!   `op ~ self` atom branches become prefix unaries, and
//!   `open ~ self ~ close` atom branches become the transparent group.
//!   The rule's own node wraps the whole climb via its `RuleDesc`.
//! - **Document sweep** — the start pattern is followed by
//!   `rep(recover)`: every token the grammar cannot place still lands
//!   in an ERROR node, which is what makes the generated parser
//!   byte-faithful on arbitrary input (`render(parse(x)) == x`).
//!
//! ## What v0 refuses (deterministic, named errors)
//!
//! Lookahead predicates (`and`/`not` markers), bounded repetitions
//! above one, transparent pratt rules, node-rule pratt branches, and
//! malformed pratt tails or atoms. Nothing is silently dropped.
//!
//! ## The token feed
//!
//! [`Lowered::feed`] bridges `lexgen::Scanner` output to the engine's
//! clean-feed contract: scanner kinds remap into the generated kind
//! space, unmatched characters become `bad_kind` tokens, trivia lexemes
//! drop (the engine re-derives trivia records from feed gaps —
//! ADR-008).
//!
//! ```no_run
//! use gir_syntax::load_grammar;
//!
//! let grammar = load_grammar(r#"
//!     language "mini" version="1.0.0"
//!     tokens { a kind=lit "a" }
//!     grammar { start kind=seq a rep }
//! "#).expect("loads");
//! let lowered = descent_lower::lower(&grammar).expect("lowers");
//! let scanner = lexgen::compile(&grammar).expect("lexes");
//! let tokens = lowered.feed(&scanner, "a a");
//! let run = descent::parse_events(&lowered.desc, &tokens, "a a");
//! assert_eq!(run.errors, 0);
//! ```

use std::collections::HashMap;
use std::ops::Range;

use descent::{Desc, InfixDesc, Pat, PrattDesc, RuleDesc, UnaryDesc};
use gir_analysis::first_follow::first_of;
use gir_analysis::{FirstSet, FirstSets, FollowSets};
use gir_ir::{Element, Grammar, RuleId, TokenId};
use lexgen::{Lexeme, Scanner};

/// The JSON parity fixture (issue 043): the reference pack's language
/// on the KDL surface — shared by the parity suites, the dhat budget
/// gates, and the criterion head-to-head benches. Value leaves are bare
/// tokens; `object`/`pair`/`array` are the only nodes.
pub const JSON_GRAMMAR: &str = include_str!("../fixtures/json.kdl");

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why a grammar cannot lower into descriptors. Every variant names the
/// offending rule — the compiler refuses, it never mis-lowers.
///
/// The set of refusals is expected to grow with the lowering (1.x
/// additions), so the enum is [`non_exhaustive`] from birth.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LowerError {
    /// The IR itself is invalid ([`Grammar::validate`] failed).
    Ir(String),
    /// A cycle among `node="false"` rules: inlining would not terminate.
    /// `rule` is the cycle's entry point.
    TransparentCycle {
        /// The rule where the cycle closed.
        rule: String,
    },
    /// A pratt-hinted rule without `node="true"`: the climb has to ride
    /// inside the rule's node.
    TransparentPratt {
        /// The pratt rule's name.
        rule: String,
    },
    /// An `and`/`not` marker: no descriptor form in v0.
    UnsupportedLookahead {
        /// The rule containing the predicate.
        rule: String,
    },
    /// A repetition with an upper bound above one (or a minimum above
    /// the expansion cap): the engine's `Rep` is unbounded.
    BoundedRepetition {
        /// The rule containing the repetition.
        rule: String,
    },
    /// Two `Alt` alternatives share a FIRST token — an LL(1) conflict;
    /// generated dispatch would be luck, not determinism.
    AltConflict {
        /// The rule owning the alternatives.
        rule: String,
        /// The contended token's name.
        token: String,
    },
    /// A nullable `Alt` branch that is not the last branch: every branch
    /// after it would be unreachable.
    UnreachableBranch {
        /// The rule owning the alternatives.
        rule: String,
        /// The zero-based position of the nullable branch.
        branch: usize,
    },
    /// A pratt rule's alternative resolves to a node rule, which cannot
    /// flatten into the pratt table.
    PrattBranchNode {
        /// The pratt rule's name.
        rule: String,
        /// The offending branch rule's name.
        branch: String,
    },
    /// A pratt tail is neither `op ~ self` (infix) nor a bare `op`
    /// (postfix).
    PrattTailShape {
        /// The pratt rule's name.
        rule: String,
    },
    /// A pratt atom is not a token pattern (only prefix and group forms
    /// may reference the rule).
    PrattAtomShape {
        /// The pratt rule's name.
        rule: String,
    },
    /// A pratt rule without any atom alternative (the language would be
    /// empty).
    PrattNoAtoms {
        /// The pratt rule's name.
        rule: String,
    },
    /// The kind space exhausted `u16` (more than 65 536 kinds).
    KindSpaceExhausted,
}

impl std::fmt::Display for LowerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ir(message) => write!(f, "invalid grammar: {message}"),
            Self::TransparentCycle { rule } => write!(
                f,
                "transparent rule cycle through `{rule}`: mark one rule node=\"true\" to break it"
            ),
            Self::TransparentPratt { rule } => write!(
                f,
                "pratt rule `{rule}` needs node=\"true\" (the climb rides inside the rule node)"
            ),
            Self::UnsupportedLookahead { rule } => {
                write!(
                    f,
                    "rule `{rule}`: and/not predicates have no descriptor form yet"
                )
            }
            Self::BoundedRepetition { rule } => write!(
                f,
                "rule `{rule}`: bounded repetitions above one have no descriptor form yet"
            ),
            Self::AltConflict { rule, token } => {
                write!(
                    f,
                    "rule `{rule}`: alternatives share FIRST token `{token}` (LL(1) conflict)"
                )
            }
            Self::UnreachableBranch { rule, branch } => write!(
                f,
                "rule `{rule}`: nullable branch {branch} is not last; later branches are unreachable"
            ),
            Self::PrattBranchNode { rule, branch } => write!(
                f,
                "pratt rule `{rule}`: branch rule `{branch}` is a node rule and cannot flatten into the table"
            ),
            Self::PrattTailShape { rule } => write!(
                f,
                "pratt rule `{rule}`: a tail is neither `op ~ expr` (infix) nor a bare `op` (postfix)"
            ),
            Self::PrattAtomShape { rule } => {
                write!(f, "pratt rule `{rule}`: an atom is not a token pattern")
            }
            Self::PrattNoAtoms { rule } => {
                write!(f, "pratt rule `{rule}` has no atom alternative")
            }
            Self::KindSpaceExhausted => write!(f, "kind space exhausted (u16)"),
        }
    }
}

impl std::error::Error for LowerError {}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// The lowering result: the descriptor program plus the kind maps a
/// pack (or a codegen emitter) needs to name its own tree.
#[derive(Debug, Clone)]
pub struct Lowered {
    /// The descriptor program: dense node rules, pratt tables, and the
    /// document start pattern (with the trailing recovery sweep).
    pub desc: Desc,
    /// Kind per token id (indexed by `TokenId`).
    pub kind_of_token: Vec<u16>,
    /// Kind per rule id; `None` marks transparent (`node="false"`)
    /// rules, which inline at their call sites (indexed by `RuleId`).
    pub kind_of_rule: Vec<Option<u16>>,
    /// The sentinel kind carried by scanner-unmatched characters —
    /// outside every declared kind, so no rule dispatches on it and
    /// recovery wraps each one in an ERROR node.
    pub bad_kind: u16,
    /// The trivia token's kind, when the grammar declares one; the
    /// tree builder interleaves trivia leaves under this kind.
    pub trivia_kind: Option<u16>,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Lower a canonical grammar into descent descriptors.
///
/// # Errors
/// [`LowerError`] for every canonical form v0 cannot express — cycles
/// among transparent rules, lookahead predicates, bounded repetitions,
/// LL(1)-conflicting alternatives, malformed pratt shapes.
pub fn lower(grammar: &Grammar) -> Result<Lowered, LowerError> {
    grammar
        .validate()
        .map_err(|error| LowerError::Ir(error.to_string()))?;
    let first = FirstSets::compute(grammar);
    let follow = FollowSets::compute(grammar, &first);

    // Kind assignment: node rules, node tokens, plain tokens, trivia —
    // each pass in declaration order; the unmatched sentinel last.
    let kinds = assign_kinds(grammar)?;
    let (kind_of_rule, kind_of_token, bad_kind, trivia_kind) =
        (kinds.of_rule, kinds.of_token, kinds.bad, kinds.trivia);

    // The transparent subgraph must be acyclic (inlining termination).
    check_transparent_acyclic(grammar)?;

    // Pratt tables first: node rules reference them by table id.
    let (exprs, pratt_table_of_rule) = build_pratt_tables(grammar, &kind_of_token)?;

    // Node rules become the dense RuleDesc table, bodies lowered below.
    let table = build_rule_table(
        grammar,
        &follow,
        &kind_of_token,
        &kind_of_rule,
        &pratt_table_of_rule,
    )?;
    let (mut rules, desc_index_of_rule) = (table.rules, table.index_of_rule);
    for rule in &grammar.rules {
        if !rule.node || pratt_table_of_rule.contains_key(&rule.id) {
            continue;
        }
        let mut ctx = Ctx {
            grammar,
            first: &first,
            kind_of_token: &kind_of_token,
            desc_index_of_rule: &desc_index_of_rule,
            current: rule.id,
        };
        let pat = pat_of(&mut ctx, &rule.body)?;
        let index = desc_index_of_rule[rule.id.0 as usize].expect("node rule indexed");
        rules[usize::from(index)].body = pat;
    }

    // The start pattern: the start rule (delegated when it is a node
    // rule, inlined when transparent) followed by the document sweep —
    // every unplaced token becomes an ERROR node, which is what keeps
    // arbitrary input byte-faithful.
    let mut ctx = Ctx {
        grammar,
        first: &first,
        kind_of_token: &kind_of_token,
        desc_index_of_rule: &desc_index_of_rule,
        current: grammar.start,
    };
    let start_pat = if let Some(index) = desc_index_of_rule[grammar.start.0 as usize] {
        Pat::Rule(index)
    } else {
        let body = grammar.rule(grammar.start).body.clone();
        pat_of(&mut ctx, &body)?
    };
    let desc = Desc {
        rules,
        exprs,
        start: Pat::Seq(vec![start_pat, Pat::Rep(Box::new(Pat::Recover))]),
    };

    Ok(Lowered {
        desc,
        kind_of_token,
        kind_of_rule,
        bad_kind,
        trivia_kind,
    })
}

/// The assigned kind maps plus the sentinel positions.
struct Kinds {
    /// Kind per rule id; `None` marks transparent rules.
    of_rule: Vec<Option<u16>>,
    /// Kind per token id.
    of_token: Vec<u16>,
    /// The unmatched-character sentinel (outside every declared kind).
    bad: u16,
    /// The trivia token's kind, when one is declared.
    trivia: Option<u16>,
}

/// The dense kind space: `node="true"` rules in declaration order, then
/// `node="true"` tokens, then plain tokens, then trivia; the
/// unmatched-character sentinel lands last.
fn assign_kinds(grammar: &Grammar) -> Result<Kinds, LowerError> {
    let mut kind_of_rule: Vec<Option<u16>> = vec![None; grammar.rules.len()];
    let mut kind_of_token: Vec<Option<u16>> = vec![None; grammar.tokens.len()];
    let mut next: u16 = 0;
    let bump = |kind: &mut Option<u16>, next: &mut u16| -> Result<(), LowerError> {
        let assigned = *next;
        *next = next.checked_add(1).ok_or(LowerError::KindSpaceExhausted)?;
        *kind = Some(assigned);
        Ok(())
    };
    for (idx, rule) in grammar.rules.iter().enumerate() {
        if rule.node {
            bump(&mut kind_of_rule[idx], &mut next)?;
        }
    }
    for (idx, token) in grammar.tokens.iter().enumerate() {
        if !token.trivia && token.node {
            bump(&mut kind_of_token[idx], &mut next)?;
        }
    }
    for (idx, token) in grammar.tokens.iter().enumerate() {
        if !token.trivia && !token.node {
            bump(&mut kind_of_token[idx], &mut next)?;
        }
    }
    for (idx, token) in grammar.tokens.iter().enumerate() {
        if token.trivia {
            bump(&mut kind_of_token[idx], &mut next)?;
        }
    }
    let trivia_kind = grammar
        .tokens
        .iter()
        .position(|token| token.trivia)
        .and_then(|idx| kind_of_token[idx]);
    let bad_kind = next;
    let kind_of_token: Vec<u16> = kind_of_token
        .into_iter()
        .map(|kind| kind.expect("every token falls into exactly one kind pass"))
        .collect();
    Ok(Kinds {
        of_rule: kind_of_rule,
        of_token: kind_of_token,
        bad: bad_kind,
        trivia: trivia_kind,
    })
}

/// The pratt tables (one per hinted rule) plus the rule → table-id map.
fn build_pratt_tables(
    grammar: &Grammar,
    kind_of_token: &[u16],
) -> Result<(Vec<PrattDesc>, HashMap<RuleId, u16>), LowerError> {
    let mut exprs: Vec<PrattDesc> = Vec::new();
    let mut pratt_table_of_rule: HashMap<RuleId, u16> = HashMap::new();
    for rule in &grammar.rules {
        let Some(hint) = rule.pratt else { continue };
        if !rule.node {
            return Err(LowerError::TransparentPratt {
                rule: grammar.rule_name(rule.id).to_owned(),
            });
        }
        let table = build_pratt_table(grammar, kind_of_token, rule.id, hint)?;
        let table_id = u16::try_from(exprs.len()).map_err(|_| LowerError::KindSpaceExhausted)?;
        pratt_table_of_rule.insert(rule.id, table_id);
        exprs.push(table);
    }
    Ok((exprs, pratt_table_of_rule))
}

/// The dense `RuleDesc` table plus its rule-id → descriptor-index map
/// (one entry per node rule; pratt rules carry `Pat::Expr`).
struct RuleTable {
    /// The descriptor entries, dense.
    rules: Vec<RuleDesc>,
    /// Descriptor index per rule id; `None` marks transparent rules.
    index_of_rule: Vec<Option<u16>>,
}

/// Build the descriptor table: one entry per node rule, bodies patched
/// after every index exists.
fn build_rule_table(
    grammar: &Grammar,
    follow: &FollowSets,
    kind_of_token: &[u16],
    kind_of_rule: &[Option<u16>],
    pratt_table_of_rule: &HashMap<RuleId, u16>,
) -> Result<RuleTable, LowerError> {
    let mut desc_index_of_rule: Vec<Option<u16>> = vec![None; grammar.rules.len()];
    let mut rules: Vec<RuleDesc> = Vec::new();
    for rule in &grammar.rules {
        if !rule.node {
            continue;
        }
        let index = u16::try_from(rules.len()).map_err(|_| LowerError::KindSpaceExhausted)?;
        desc_index_of_rule[rule.id.0 as usize] = Some(index);
        let body = if let Some(table_id) = pratt_table_of_rule.get(&rule.id) {
            Pat::Expr(*table_id)
        } else {
            Pat::Seq(Vec::new())
        };
        rules.push(RuleDesc {
            node_kind: kind_of_rule[rule.id.0 as usize].expect("node rule has a kind"),
            body,
            sync: sync_set(follow, kind_of_token, rule),
            mode_hook: None,
        });
    }
    Ok(RuleTable {
        rules,
        index_of_rule: desc_index_of_rule,
    })
}

// ---------------------------------------------------------------------------
// Token feed bridge
// ---------------------------------------------------------------------------

impl Lowered {
    /// Bridge a compiled scanner to the engine's clean-feed contract:
    /// scanner kinds remap into the generated kind space, unmatched
    /// characters become [`Self::bad_kind`] tokens, and trivia lexemes
    /// drop (the engine re-derives trivia records from feed gaps —
    /// ADR-008).
    #[must_use]
    pub fn feed(&self, scanner: &Scanner, source: &str) -> Vec<(u16, Range<u32>)> {
        scanner
            .scan(source)
            .into_iter()
            .filter_map(|lexeme| match lexeme {
                Lexeme::Token {
                    kind,
                    range,
                    trivia: false,
                } => Some((self.kind_of_token[usize::from(kind)], range)),
                Lexeme::Error { range } => Some((self.bad_kind, range)),
                Lexeme::Token { trivia: true, .. } => None,
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Transparent acyclicity
// ---------------------------------------------------------------------------

/// Inlining walks transparent rule bodies, so the transparent subgraph
/// (`node="false"`, no pratt hint) must be acyclic. Recursion depth is
/// bounded by the rule count (cycles are cut off by the gray mark).
fn check_transparent_acyclic(grammar: &Grammar) -> Result<(), LowerError> {
    fn visit(id: RuleId, grammar: &Grammar, state: &mut [u8]) -> Result<(), LowerError> {
        if state[id.0 as usize] == 2 {
            return Ok(());
        }
        state[id.0 as usize] = 1;
        let rule = grammar.rule(id);
        if !rule.node && rule.pratt.is_none() {
            for target in rule.body.rule_refs() {
                if state[target.0 as usize] == 1 {
                    return Err(LowerError::TransparentCycle {
                        rule: grammar.rule_name(target).to_owned(),
                    });
                }
                visit(target, grammar, state)?;
            }
        }
        state[id.0 as usize] = 2;
        Ok(())
    }
    let mut state: Vec<u8> = vec![0; grammar.rules.len()];
    for rule in &grammar.rules {
        visit(rule.id, grammar, &mut state)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Sync sets
// ---------------------------------------------------------------------------

/// The recovery stop set: declared `sync` tokens when present, else the
/// rule's FOLLOW set (trivia excluded — the feed never carries it).
fn sync_set(follow: &FollowSets, kind_of_token: &[u16], rule: &gir_ir::Rule) -> Vec<u16> {
    let mut kinds: Vec<u16> = if rule.sync.is_empty() {
        follow.sets[rule.id.0 as usize]
            .tokens
            .iter()
            .map(|id| kind_of_token[usize::from(id.0)])
            .collect()
    } else {
        rule.sync
            .iter()
            .map(|id| kind_of_token[usize::from(id.0)])
            .collect()
    };
    kinds.sort_unstable();
    kinds.dedup();
    kinds
}

// ---------------------------------------------------------------------------
// Element lowering
// ---------------------------------------------------------------------------

/// The lowering context: what a recursive body needs to name its parts.
struct Ctx<'g> {
    grammar: &'g Grammar,
    first: &'g FirstSets,
    kind_of_token: &'g [u16],
    desc_index_of_rule: &'g [Option<u16>],
    /// The rule whose body is being lowered (error attribution).
    current: RuleId,
}

impl Ctx<'_> {
    /// The current rule's display name.
    fn name(&self) -> String {
        self.grammar.rule_name(self.current).to_owned()
    }

    /// A token's feed kind, filtering trivia out of dispatch sets.
    fn feed_kind(&self, id: TokenId) -> Option<u16> {
        let token = self.grammar.token(id);
        if token.trivia {
            None
        } else {
            Some(self.kind_of_token[usize::from(id.0)])
        }
    }

    /// The token name for error messages.
    fn token_name(&self, kind: u16) -> String {
        self.grammar
            .tokens
            .iter()
            .find(|token| self.kind_of_token[token.id.0 as usize] == kind)
            .map_or_else(
                || format!("kind{kind}"),
                |token| self.grammar.token_name(token.id).to_owned(),
            )
    }
}

/// Lower one IR element into a descriptor pattern.
fn pat_of(ctx: &mut Ctx<'_>, element: &Element) -> Result<Pat, LowerError> {
    match element {
        Element::Token(id) => {
            let kind = ctx.kind_of_token[usize::from(id.0)];
            let token = ctx.grammar.token(*id);
            if token.node && !token.trivia {
                Ok(Pat::Node(kind, Box::new(Pat::Tok(kind))))
            } else {
                Ok(Pat::Tok(kind))
            }
        }
        Element::Rule(id) => {
            // Node rules (pratt or plain) delegate; the engine emits the
            // rule's own node. Transparent rules were proven acyclic —
            // inline.
            if let Some(index) = ctx.desc_index_of_rule[id.0 as usize] {
                return Ok(Pat::Rule(index));
            }
            let body = ctx.grammar.rule(*id).body.clone();
            let previous = ctx.current;
            ctx.current = *id;
            let pat = pat_of(ctx, &body);
            ctx.current = previous;
            pat
        }
        Element::Seq(elements) => {
            let mut pats = Vec::with_capacity(elements.len());
            for inner in elements {
                pats.push(guarded(ctx, inner)?);
            }
            Ok(Pat::Seq(pats))
        }
        Element::Alt(branches) => alt_pat(ctx, branches),
        Element::Opt(inner) => Ok(Pat::Opt(Box::new(guarded(ctx, inner)?))),
        Element::Rep { inner, min, max } => rep_pat(ctx, *min, *max, inner),
        Element::And(_) | Element::Not(_) => {
            Err(LowerError::UnsupportedLookahead { rule: ctx.name() })
        }
    }
}

/// Repetition shapes: `*` and `+` lower directly, an upper bound of one
/// is the optional form, small bare minimums expand deterministically,
/// everything bounded above one is refused.
fn rep_pat(
    ctx: &mut Ctx<'_>,
    min: u32,
    max: Option<u32>,
    inner: &Element,
) -> Result<Pat, LowerError> {
    match (min, max) {
        (0, None) => Ok(Pat::Rep(Box::new(guarded(ctx, inner)?))),
        (1, None) => {
            let first = guarded(ctx, inner)?;
            let rest = guarded(ctx, inner)?;
            Ok(Pat::Seq(vec![first, Pat::Rep(Box::new(rest))]))
        }
        (_, Some(1)) => Ok(Pat::Opt(Box::new(guarded(ctx, inner)?))),
        (min, None) if (2..=MIN_EXPANSION_CAP).contains(&min) => {
            let mut pats = Vec::with_capacity(usize::try_from(min).expect("capped"));
            for _ in 0..min {
                pats.push(guarded(ctx, inner)?);
            }
            let tail = guarded(ctx, inner)?;
            pats.push(Pat::Rep(Box::new(tail)));
            Ok(Pat::Seq(pats))
        }
        _ => Err(LowerError::BoundedRepetition { rule: ctx.name() }),
    }
}

/// The deterministic expansion cap for `Rep { min: n, max: None }`.
const MIN_EXPANSION_CAP: u32 = 8;

/// LL(1) dispatch: every rule-headed branch gets a FIRST guard, token
/// branches stay bare, overlaps are conflicts, nullable branches must
/// be last.
fn alt_pat(ctx: &mut Ctx<'_>, branches: &[Element]) -> Result<Pat, LowerError> {
    let mut pats = Vec::with_capacity(branches.len());
    let mut seen_firsts: Vec<Vec<u16>> = Vec::with_capacity(branches.len());
    for (position, branch) in branches.iter().enumerate() {
        let mut set = FirstSet::default();
        first_of(branch, &ctx.first.sets, &mut set);
        if set.opaque {
            return Err(LowerError::UnsupportedLookahead { rule: ctx.name() });
        }
        let kinds: Vec<u16> = set
            .tokens
            .iter()
            .filter_map(|id| ctx.feed_kind(*id))
            .collect();
        // Nullable branches must close the choice.
        if set.nullable && position + 1 != branches.len() {
            return Err(LowerError::UnreachableBranch {
                rule: ctx.name(),
                branch: position,
            });
        }
        // LL(1) conflict: a token claimed by two branches.
        let clash = seen_firsts
            .iter()
            .flatten()
            .find(|kind| kinds.contains(kind));
        if let Some(kind) = clash {
            return Err(LowerError::AltConflict {
                rule: ctx.name(),
                token: ctx.token_name(*kind),
            });
        }
        seen_firsts.push(kinds.clone());
        let pat = guarded(ctx, branch)?;
        if head_reaches_rule(ctx, branch) {
            pats.push(dispatch(kinds, pat));
        } else {
            pats.push(pat);
        }
    }
    Ok(Pat::Alt(pats))
}

/// FIRST-guard a rule-headed body (token heads dispatch by the exact
/// `Tok` check already, and a guard on them would only bloat the
/// descriptor): the guard keeps valid input from speculatively entering
/// a self-recovering rule.
fn guarded(ctx: &mut Ctx<'_>, element: &Element) -> Result<Pat, LowerError> {
    let pat = pat_of(ctx, element)?;
    if !head_reaches_rule(ctx, element) {
        return Ok(pat);
    }
    let mut set = FirstSet::default();
    first_of(element, &ctx.first.sets, &mut set);
    let kinds: Vec<u16> = set
        .tokens
        .iter()
        .filter_map(|id| ctx.feed_kind(*id))
        .collect();
    if set.nullable || kinds.is_empty() {
        // Nothing to guard on: the engine's zero-progress break stops
        // the loop instead.
        return Ok(pat);
    }
    Ok(dispatch(kinds, pat))
}

/// FIRST-guarded dispatch: `Peek` for a singleton, `PeekSet` beyond.
fn dispatch(mut kinds: Vec<u16>, pat: Pat) -> Pat {
    kinds.sort_unstable();
    kinds.dedup();
    if kinds.len() == 1 {
        Pat::Peek {
            tok: kinds[0],
            pat: Box::new(pat),
        }
    } else {
        Pat::PeekSet {
            toks: kinds,
            pat: Box::new(pat),
        }
    }
}

/// Whether a pattern's head can reach a rule reference (the condition
/// for FIRST-guarding): token heads never need a guard. A nullable
/// prefix element is transparent for this purpose — the first
/// non-nullable head decides.
fn head_reaches_rule(ctx: &Ctx<'_>, element: &Element) -> bool {
    match element {
        Element::Rule(_) => true,
        Element::Token(_) | Element::Not(_) => false,
        Element::Seq(elements) => {
            for inner in elements {
                let mut set = FirstSet::default();
                first_of(inner, &ctx.first.sets, &mut set);
                if head_reaches_rule(ctx, inner) {
                    return true;
                }
                if !set.nullable {
                    return false;
                }
            }
            false
        }
        Element::Alt(branches) => branches.iter().any(|branch| head_reaches_rule(ctx, branch)),
        Element::Opt(inner) | Element::Rep { inner, .. } | Element::And(inner) => {
            head_reaches_rule(ctx, inner)
        }
    }
}

// ---------------------------------------------------------------------------
// Pratt tables
// ---------------------------------------------------------------------------

/// Build the pratt table for one hinted rule: leading-self branches are
/// the infix/postfix tails, the remaining branches are atoms (with
/// prefix and group forms detected among them). Operator nodes reuse
/// the operator token's kind; the rule's node wraps the whole climb.
fn build_pratt_table(
    grammar: &Grammar,
    kind_of_token: &[u16],
    rule_id: RuleId,
    hint: gir_ir::PrattHint,
) -> Result<PrattDesc, LowerError> {
    let rule = grammar.rule(rule_id);
    let rule_name = grammar.rule_name(rule_id).to_owned();
    let branches: Vec<Element> = match &rule.body {
        Element::Alt(branches) => branches.clone(),
        single => vec![single.clone()],
    };

    let (tails, atoms) = classify_branches(grammar, &branches, rule_id, &rule_name)?;
    if atoms.is_empty() {
        return Err(LowerError::PrattNoAtoms { rule: rule_name });
    }
    let (infix, postfix) = build_tails(kind_of_token, &tails, rule_id, hint, &rule_name)?;
    let atom_forms = build_atoms(grammar, kind_of_token, &atoms, rule_id, &rule_name)?;
    let (group, prefix, mut atom_pats) = (atom_forms.group, atom_forms.prefix, atom_forms.pats);

    // Prefix/postfix unaries bind tighter than every infix level, so
    // the operand climb never swallows a same-level operator.
    let max_lbp = infix.iter().map(|entry| entry.lbp).max().unwrap_or(0);
    let unary_bp = max_lbp.saturating_add(1);
    let (mut prefix, mut postfix) = (prefix, postfix);
    for unary in prefix.iter_mut().chain(postfix.iter_mut()) {
        unary.bp = unary_bp;
    }

    let atom = if atom_pats.len() == 1 {
        atom_pats.remove(0)
    } else {
        Pat::Alt(atom_pats)
    };
    Ok(PrattDesc {
        atom,
        group,
        prefix,
        postfix,
        infix,
    })
}

/// A pratt branch must flatten to a token pattern: transparent rules
/// dereference, node rules cannot.
fn deref_branch(
    grammar: &Grammar,
    element: &Element,
    self_id: RuleId,
    rule_name: &str,
) -> Result<Element, LowerError> {
    match element {
        Element::Rule(id) if *id != self_id => {
            let target = grammar.rule(*id);
            if target.node {
                return Err(LowerError::PrattBranchNode {
                    rule: rule_name.to_owned(),
                    branch: grammar.rule_name(*id).to_owned(),
                });
            }
            deref_branch(grammar, &target.body.clone(), self_id, rule_name)
        }
        other => Ok(other.clone()),
    }
}

/// Split the rule's alternatives into leading-self tails (infix/postfix)
/// and atoms.
fn classify_branches(
    grammar: &Grammar,
    branches: &[Element],
    rule_id: RuleId,
    rule_name: &str,
) -> Result<(Vec<Vec<Element>>, Vec<Element>), LowerError> {
    let mut tails: Vec<Vec<Element>> = Vec::new();
    let mut atoms: Vec<Element> = Vec::new();
    for branch in branches {
        let resolved = deref_branch(grammar, branch, rule_id, rule_name)?;
        // A leading self-reference makes the branch an infix/postfix
        // tail; anything else is an atom.
        let rest: Option<Vec<Element>> = match &resolved {
            Element::Seq(elements) => match elements.first() {
                Some(Element::Rule(id)) if *id == rule_id => Some(elements[1..].to_vec()),
                _ => None,
            },
            Element::Rule(id) if *id == rule_id => Some(Vec::new()),
            _ => None,
        };
        match rest {
            Some(rest) => tails.push(rest),
            None => atoms.push(resolved),
        }
    }
    Ok((tails, atoms))
}

/// Lower the tails: `op ~ self` becomes an infix entry, a bare `op` a
/// postfix entry; anything else is a shape refusal.
fn build_tails(
    kind_of_token: &[u16],
    tails: &[Vec<Element>],
    rule_id: RuleId,
    hint: gir_ir::PrattHint,
    rule_name: &str,
) -> Result<(Vec<InfixDesc>, Vec<UnaryDesc>), LowerError> {
    let token_kind = |id: TokenId| kind_of_token[usize::from(id.0)];
    let mut infix: Vec<InfixDesc> = Vec::new();
    let mut postfix: Vec<UnaryDesc> = Vec::new();
    for tail in tails {
        // A factored tail may itself be an alternation of operator
        // rest-shapes: flatten one level.
        let rest_shapes: Vec<Vec<Element>> = match tail.as_slice() {
            [] => {
                return Err(LowerError::PrattTailShape {
                    rule: rule_name.to_owned(),
                });
            }
            [Element::Alt(alts)] => alts
                .iter()
                .map(|alt| match alt {
                    Element::Seq(elements) => elements.clone(),
                    single => vec![single.clone()],
                })
                .collect(),
            [Element::Seq(elements)] => vec![elements.clone()],
            slice => vec![slice.to_vec()],
        };
        for rest in rest_shapes {
            match rest.as_slice() {
                [Element::Token(op)] => postfix.push(UnaryDesc {
                    token: token_kind(*op),
                    node: token_kind(*op),
                    bp: 0,
                }),
                [Element::Token(op), Element::Rule(id)] if *id == rule_id => {
                    infix.push(InfixDesc {
                        token: token_kind(*op),
                        lbp: hint.left_bp,
                        rbp: hint.right_bp,
                        node: token_kind(*op),
                    });
                }
                _ => {
                    return Err(LowerError::PrattTailShape {
                        rule: rule_name.to_owned(),
                    });
                }
            }
        }
    }
    Ok((infix, postfix))
}

/// The peeled atom alternatives: the transparent group, prefix unaries,
/// and the remaining token patterns.
struct AtomForms {
    /// The `(open, close)` token kinds of the transparent group.
    group: Option<(u16, u16)>,
    /// Prefix unary descriptors.
    prefix: Vec<UnaryDesc>,
    /// The remaining atom patterns.
    pats: Vec<Pat>,
}

/// Peel the atom alternatives: `op ~ self` is a prefix unary,
/// `open ~ self ~ close` the transparent group (first one wins), the
/// rest lower as token patterns.
fn build_atoms(
    grammar: &Grammar,
    kind_of_token: &[u16],
    atoms: &[Element],
    rule_id: RuleId,
    rule_name: &str,
) -> Result<AtomForms, LowerError> {
    let token_kind = |id: TokenId| kind_of_token[usize::from(id.0)];
    let mut group: Option<(u16, u16)> = None;
    let mut prefix: Vec<UnaryDesc> = Vec::new();
    let mut atom_pats: Vec<Pat> = Vec::new();
    for atom in atoms {
        match atom {
            Element::Seq(elements)
                if elements.len() == 2
                    && matches!(&elements[0], Element::Token(_))
                    && matches!(&elements[1], Element::Rule(id) if *id == rule_id) =>
            {
                let Element::Token(op) = &elements[0] else {
                    continue;
                };
                prefix.push(UnaryDesc {
                    token: token_kind(*op),
                    node: token_kind(*op),
                    bp: 0,
                });
            }
            Element::Seq(elements)
                if elements.len() == 3
                    && matches!(&elements[0], Element::Token(_))
                    && matches!(&elements[1], Element::Rule(id) if *id == rule_id)
                    && matches!(&elements[2], Element::Token(_))
                    && group.is_none() =>
            {
                let (Element::Token(open), Element::Token(close)) = (&elements[0], &elements[2])
                else {
                    continue;
                };
                group = Some((token_kind(*open), token_kind(*close)));
            }
            other => atom_pats.push(atom_pat(grammar, kind_of_token, other, rule_name)?),
        }
    }
    Ok(AtomForms {
        group,
        prefix,
        pats: atom_pats,
    })
}

/// Lower a pratt atom branch (prefix and group forms were already peeled
/// off by the caller): token patterns only.
fn atom_pat(
    grammar: &Grammar,
    kind_of_token: &[u16],
    element: &Element,
    rule_name: &str,
) -> Result<Pat, LowerError> {
    match element {
        Element::Token(id) => {
            let kind = kind_of_token[usize::from(id.0)];
            let token = grammar.token(*id);
            if token.node && !token.trivia {
                Ok(Pat::Node(kind, Box::new(Pat::Tok(kind))))
            } else {
                Ok(Pat::Tok(kind))
            }
        }
        Element::Alt(branches) => {
            let pats = branches
                .iter()
                .map(|branch| atom_pat(grammar, kind_of_token, branch, rule_name))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Pat::Alt(pats))
        }
        Element::Seq(elements) => {
            let pats = elements
                .iter()
                .map(|branch| atom_pat(grammar, kind_of_token, branch, rule_name))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Pat::Seq(pats))
        }
        _ => Err(LowerError::PrattAtomShape {
            rule: rule_name.to_owned(),
        }),
    }
}
