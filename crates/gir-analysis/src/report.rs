//! The determinism report (issue 012): per-rule LL(1)/Pratt verdicts,
//! conflict sites, backend classification, and the ambiguity marks
//! (roadmap §6.2 / ADR-007) consumed by codegen and the CLI.

use std::collections::BTreeSet;
use std::fmt;

use gir_ir::{Grammar, RuleId, TokenId};
use lr_shadow::ConflictKind;

use crate::first_follow::{FirstSets, FollowSets};
use crate::ll1::Obstruction;

/// Which backend generates the code for a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend {
    /// Pratt precedence-climbing (rule carries a pratt hint; conflicts
    /// inside expression regions are expected and fine).
    Pratt,
    /// Deterministic recursive descent (LL(1)-dispatchable).
    Descent,
    /// Table-driven fallback (rule needs more than one-token lookahead
    /// or contains opaque constructs).
    Table,
    /// GLR island candidate (roadmap §6.2, ADR-007): the shadow automaton
    /// marks the rule's region ambiguous and the grammar carries
    /// `@allow_ambiguous` on it — the build continues with an island.
    /// Only annotated table-fallback regions reclassify.
    GlrIsland,
}

impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pratt => write!(f, "pratt"),
            Self::Descent => write!(f, "descent"),
            Self::Table => write!(f, "table"),
            Self::GlrIsland => write!(f, "island"),
        }
    }
}

/// One conflict site: rule, contended token, branch pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConflictSite {
    /// The rule with the conflict.
    pub rule: RuleId,
    /// The contended token.
    pub token: gir_ir::TokenId,
    /// Branch indices.
    pub branches: (usize, usize),
}

/// The verdict for one rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleVerdict {
    /// The rule.
    pub rule: RuleId,
    /// Backend classification.
    pub backend: Backend,
    /// Whether the rule is LL(1)-dispatchable.
    pub ll1: bool,
    /// Whether the rule is (indirectly) left-recursive — descent codegen
    /// rejects these unless a pratt hint lowered them.
    pub left_recursive: bool,
    /// Rule references only reachable rules see (informational).
    pub reachable: bool,
}

/// Full determinism classification for a grammar (issue 012): consumed
/// by codegen and by `syntact check`/`conflicts`.
#[derive(Debug, Clone)]
pub struct DeterminismReport {
    /// Per-rule verdicts, in rule-id order.
    pub verdicts: Vec<RuleVerdict>,
    /// Every LL(1) conflict site (unfiltered — pratt rules may carry
    /// conflicts that do not force the table backend).
    pub conflicts: Vec<ConflictSite>,
    /// Ambiguous regions (roadmap §6.2): shadow-automaton conflicts
    /// attributed to rules, with the two counter-example derivations.
    /// Present only when the report is computed against the automaton
    /// ([`DeterminismReport::compute_with_ambiguities`]).
    pub ambiguities: Vec<Ambiguity>,
}

/// One ambiguous region: a shadow-automaton conflict attributed to a
/// rule, the two contending derivations, and whether the grammar
/// accepted the region as an island candidate (`@allow_ambiguous`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ambiguity {
    /// The attributed rule (the first declaring origin of the contending
    /// pair).
    pub rule: RuleId,
    /// The contended token (`None` for the pseudo-EOF).
    pub term: Option<TokenId>,
    /// How the automaton resolved the contention.
    pub kind: ConflictKind,
    /// Whether the region was accepted as a GLR island candidate
    /// (`@allow_ambiguous` on the attributed rule or an ancestor).
    pub allowed: bool,
    /// The two counter-example derivations, rendered `lhs -> symbols`.
    pub sides: Vec<String>,
}

impl DeterminismReport {
    /// Classify a grammar: FIRST/FOLLOW fixpoints, LL(1) verdicts, left
    /// recursion, backend selection.
    ///
    /// # Panics
    /// Never for a grammar that went through [`crate`] lowering: every
    /// rule receives an LL(1) verdict before the lookup.
    #[must_use]
    pub fn compute(grammar: &Grammar) -> Self {
        let first = FirstSets::compute(grammar);
        let follow = FollowSets::compute(grammar, &first);
        let verdicts_ll1 = crate::ll1::ll1_verdicts(grammar, &first, &follow);
        let left_recursive = left_recursion_with_first(grammar, &first);
        let unreachable: BTreeSet<RuleId> = grammar.unreachable_rules().into_iter().collect();

        let mut conflicts = Vec::new();
        let verdicts: Vec<RuleVerdict> = grammar
            .rules
            .iter()
            .map(|rule| {
                let (_, ll1) = verdicts_ll1
                    .iter()
                    .find(|(id, _)| *id == rule.id)
                    .expect("verdict exists for every rule");
                if let Some(Obstruction::Conflicts(list)) = &ll1.obstruction {
                    for conflict in list {
                        conflicts.push(ConflictSite {
                            rule: rule.id,
                            token: conflict.token,
                            branches: conflict.branches,
                        });
                    }
                }
                let backend = if rule.pratt.is_some() {
                    Backend::Pratt
                } else if ll1.ok {
                    Backend::Descent
                } else {
                    Backend::Table
                };
                RuleVerdict {
                    rule: rule.id,
                    backend,
                    ll1: ll1.ok,
                    left_recursive: left_recursive[rule.id.0 as usize],
                    reachable: !unreachable.contains(&rule.id),
                }
            })
            .collect();
        Self {
            verdicts,
            conflicts,
            ambiguities: Vec::new(),
        }
    }

    /// Classify a grammar against its shadow automaton (roadmap §6.1
    /// pass 4): the automaton's recorded conflicts attribute back to
    /// rules ([`lr_shadow::ambiguity_marks`]), propagate to enclosing
    /// regions, and render as ambiguities with two counter-example
    /// derivations. A rule carrying `@allow_ambiguous` whose region is
    /// marked reclassifies from table fallback to a GLR island candidate
    /// (ADR-007) — the build continues; unannotated regions keep the
    /// fallback.
    ///
    /// # Panics
    /// Never for a grammar that went through [`crate`] lowering.
    #[must_use]
    pub fn compute_with_ambiguities(grammar: &Grammar, automaton: &lr_shadow::Automaton) -> Self {
        let mut report = Self::compute(grammar);
        let marks = lr_shadow::ambiguity_marks(automaton);
        if marks.is_empty() {
            return report;
        }

        // Region propagation (§6.2 "the automaton restricted to that
        // region"): a mark inside a rule's call subtree marks the
        // enclosing region too.
        let marked = marked_regions(grammar, marks.iter().map(|mark| mark.rule));

        // The annotation covers a mark when any `@allow_ambiguous` rule's
        // region contains the marked rule.
        let islands: Vec<BTreeSet<RuleId>> = grammar
            .rules
            .iter()
            .filter(|rule| rule.allow_ambiguous)
            .map(|rule| region_of(grammar, rule.id))
            .collect();

        for mark in &marks {
            let allowed = islands.iter().any(|region| region.contains(&mark.rule));
            let sides: Vec<String> = mark
                .sides
                .iter()
                .map(|side| render_side(grammar, side))
                .collect();
            report.ambiguities.push(Ambiguity {
                rule: mark.rule,
                term: mark.term.as_token(),
                kind: mark.kind,
                allowed,
                sides,
            });
        }

        // The annotation acts on table-fallback regions: an annotated
        // rule whose region is marked becomes an island candidate.
        for verdict in &mut report.verdicts {
            if verdict.backend == Backend::Table
                && marked.contains(&verdict.rule)
                && grammar.rule(verdict.rule).allow_ambiguous
            {
                verdict.backend = Backend::GlrIsland;
            }
        }
        report
    }

    /// The backend chosen for a rule.
    #[must_use]
    pub fn backend_for(&self, rule: RuleId) -> Backend {
        self.verdicts
            .iter()
            .find(|verdict| verdict.rule == rule)
            .map_or(Backend::Table, |verdict| verdict.backend)
    }

    /// Whether every reachable rule classifies as deterministic
    /// (descent or pratt) — the grammar generates without table fallback.
    #[must_use]
    pub fn all_deterministic(&self) -> bool {
        self.verdicts
            .iter()
            .filter(|verdict| verdict.reachable)
            .all(|verdict| !matches!(verdict.backend, Backend::Table))
    }
}

impl fmt::Display for DeterminismReport {
    /// Deterministic snapshot form (issue 012 report snapshots):
    /// one line per rule, one per LL(1) conflict, then the ambiguity
    /// sections with their two derivations, in stable order.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for verdict in &self.verdicts {
            writeln!(
                f,
                "rule {} backend={} ll1={} left_recursive={} reachable={}",
                verdict.rule.0,
                verdict.backend,
                verdict.ll1,
                verdict.left_recursive,
                verdict.reachable
            )?;
        }
        for conflict in &self.conflicts {
            writeln!(
                f,
                "conflict rule {} token {} branches ({}, {})",
                conflict.rule.0, conflict.token.0, conflict.branches.0, conflict.branches.1
            )?;
        }
        for ambiguity in &self.ambiguities {
            let term = ambiguity
                .term
                .map_or_else(|| "eof".to_owned(), |token| format!("tok{}", token.0));
            let kind = match ambiguity.kind {
                ConflictKind::ShiftReduce => "shift/reduce",
                ConflictKind::ReduceReduce => "reduce/reduce",
            };
            writeln!(
                f,
                "ambiguity rule {} {} {} allowed={}",
                ambiguity.rule.0, term, kind, ambiguity.allowed
            )?;
            for (index, side) in ambiguity.sides.iter().enumerate().take(2) {
                writeln!(f, "  interp #{}: {}", index + 1, side)?;
            }
        }
        Ok(())
    }
}

/// Render one contending production as `lhs -> symbols` (grammar names
/// for rules and tokens; synthetic helpers keep their `ntN` spelling).
fn render_side(grammar: &Grammar, side: &lr_shadow::ProductionSide) -> String {
    let lhs = if (side.lhs.0 as usize) < grammar.rules.len() {
        grammar.rule_name(gir_ir::RuleId(side.lhs.0)).to_owned()
    } else {
        format!("nt{}", side.lhs.0)
    };
    let symbols: Vec<String> = side
        .symbols
        .iter()
        .map(|symbol| match symbol {
            lr_shadow::Symbol::Token(token) => grammar.token_name(*token).to_owned(),
            lr_shadow::Symbol::Nonterm(nonterm) => {
                if (nonterm.0 as usize) < grammar.rules.len() {
                    grammar.rule_name(gir_ir::RuleId(nonterm.0)).to_owned()
                } else {
                    format!("nt{}", nonterm.0)
                }
            }
            lr_shadow::Symbol::Eof => "eof".to_owned(),
        })
        .collect();
    if symbols.is_empty() {
        format!("{lhs} -> <empty>")
    } else {
        format!("{lhs} -> {}", symbols.join(" "))
    }
}

/// The rules whose region contains a mark: the marked rules plus every
/// ancestor in the rule-reference graph (§6.2 "the automaton restricted
/// to that region").
fn marked_regions(grammar: &Grammar, marks: impl Iterator<Item = RuleId>) -> BTreeSet<RuleId> {
    let mut reverse: BTreeSet<(RuleId, RuleId)> = BTreeSet::new();
    for rule in &grammar.rules {
        for target in rule.body.rule_refs() {
            reverse.insert((target, rule.id));
        }
    }
    let mut out = BTreeSet::new();
    let mut work: Vec<RuleId> = marks.collect();
    while let Some(id) = work.pop() {
        if out.insert(id) {
            for (target, outer) in &reverse {
                if *target == id && out.insert(*outer) {
                    work.push(*outer);
                }
            }
        }
    }
    out
}

/// A rule's region: the rule itself plus everything its body can reach.
fn region_of(grammar: &Grammar, outer: RuleId) -> BTreeSet<RuleId> {
    let mut out = BTreeSet::new();
    let mut work = vec![outer];
    while let Some(id) = work.pop() {
        if out.insert(id) {
            if let Some(rule) = grammar.try_rule(id) {
                work.extend(rule.body.rule_refs());
            }
        }
    }
    out
}

/// Left-recursion (direct + indirect): rule `a` is left-recursive when a
/// left-corner cycle leads back to it. A per-node cycle search over the
/// left-corner graph — O(rules) per node, linear on chains and O(n²)
/// even in the dense worst case (a boolean-matrix Warshall closure would
/// be cubic, which stalled `syntact build` for tens of seconds on deep
/// rule chains).
#[must_use]
pub fn left_recursion(grammar: &Grammar) -> Vec<bool> {
    let first = FirstSets::compute(grammar);
    left_recursion_with_first(grammar, &first)
}

/// [`left_recursion`] with caller-provided FIRST sets (avoids the second
/// fixpoint when the report already computed one).
#[must_use]
pub fn left_recursion_with_first(grammar: &Grammar, first: &FirstSets) -> Vec<bool> {
    let n = grammar.rules.len();
    // begins_with edges: a's body has b as a left corner (through
    // nullable prefixes).
    let mut edges: Vec<std::collections::BTreeSet<usize>> =
        vec![std::collections::BTreeSet::new(); n];
    for rule in &grammar.rules {
        let mut corners: Vec<RuleId> = Vec::new();
        left_corners(&rule.body, &first.sets, &mut corners);
        for corner in corners {
            if let Ok(idx) = usize::try_from(corner.0) {
                if idx < n {
                    edges[rule.id.0 as usize].insert(idx);
                }
            }
        }
    }
    // Rule i is left-recursive when i is reachable from i through at
    // least one left-corner edge (a cycle through i).
    let mut left_recursive = vec![false; n];
    let mut seen = vec![false; n];
    for start in 0..n {
        seen.fill(false);
        let mut work: Vec<usize> = edges[start].iter().copied().collect();
        while let Some(node) = work.pop() {
            if node == start {
                left_recursive[start] = true;
                break;
            }
            if seen[node] {
                continue;
            }
            seen[node] = true;
            work.extend(edges[node].iter().copied());
        }
    }
    left_recursive
}

/// Collect the left corners of an element: rule references reachable
/// through nullable leading positions.
fn left_corners(
    element: &gir_ir::Element,
    first: &[crate::first_follow::FirstSet],
    out: &mut Vec<RuleId>,
) {
    match element {
        gir_ir::Element::Rule(id) => out.push(*id),
        gir_ir::Element::Token(_) | gir_ir::Element::Not(_) => {}
        gir_ir::Element::Seq(elements) => {
            for element in elements {
                left_corners(element, first, out);
                // Keep scanning only through nullable prefixes (AND
                // lookaheads consume nothing: they stay in the corner
                // chain too).
                let mut set = crate::first_follow::FirstSet::default();
                crate::first_follow::first_of(element, first, &mut set);
                let nullable = set.nullable || set.opaque;
                if !nullable {
                    break;
                }
            }
        }
        gir_ir::Element::Alt(branches) => {
            for branch in branches {
                left_corners(branch, first, out);
            }
        }
        gir_ir::Element::Opt(inner) | gir_ir::Element::Rep { inner, .. } => {
            left_corners(inner, first, out);
        }
        gir_ir::Element::And(inner) => left_corners(inner, first, out),
    }
}
