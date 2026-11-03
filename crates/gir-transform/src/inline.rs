//! Cycle-guarded inlining (issue 013): rules that are referenced exactly
//! once and carry no parse-relevant annotation are folded into their only
//! call site — unless the fold would introduce recursion.
//!
//! Guards (each alone skips the candidate):
//! - the candidate is the start rule or is annotated (`@node`, `@pratt`,
//!   `@list`, cold, fragile, `wrap_error`, sync/insert sets) — such rules
//!   carry observable parse behavior;
//! - the candidate references itself (folding would loop);
//! - the candidate reaches its caller transitively (folding would create
//!   a cycle that did not exist textually before).
//!
//! Inlined rules become unreachable and stay in place: rule ids are
//! dense and stable, so removal would force a global renumbering for no
//! parsing benefit (`Grammar::unreachable_rules` reports them).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use gir_ir::{Element, Grammar, RuleId};

/// Inline all eligible single-use rules, to a fixpoint.
#[must_use]
pub fn inline_single_use(grammar: &Grammar) -> Grammar {
    let mut current = grammar.clone();
    loop {
        let next = one_inline_pass(&current);
        if next == current {
            return next;
        }
        current = next;
    }
}

/// One pass: find the first eligible candidate and inline it once.
fn one_inline_pass(grammar: &Grammar) -> Grammar {
    let uses = count_uses(grammar);
    for candidate in &grammar.rules {
        if !is_candidate(grammar, candidate.id, &uses) {
            continue;
        }
        let sites = &uses[&candidate.id];
        let caller = sites[0];
        // Cycle guard: the candidate must not reach the caller (or
        // itself) through its own references.
        let forbidden = BTreeSet::from([caller, candidate.id]);
        if reaches(grammar, candidate.id, &forbidden) {
            continue;
        }
        let body = candidate.body.clone();
        let mut out = grammar.clone();
        let caller_idx = caller.0 as usize;
        let caller_body = out.rules[caller_idx].body.clone();
        out.rules[caller_idx].body = replace_rule_ref(&caller_body, candidate.id, &body);
        return out;
    }
    grammar.clone()
}

/// Callers per rule id: one entry per textual occurrence (not deduplicated
/// — `s → x | x` counts two uses of `x`).
fn count_uses(grammar: &Grammar) -> BTreeMap<RuleId, Vec<RuleId>> {
    let mut uses: BTreeMap<RuleId, Vec<RuleId>> = BTreeMap::new();
    for rule in &grammar.rules {
        count_in(&rule.body, rule.id, &mut uses);
    }
    uses
}

/// Walk one element tree, recording every rule-reference occurrence.
fn count_in(element: &Element, caller: RuleId, uses: &mut BTreeMap<RuleId, Vec<RuleId>>) {
    match element {
        Element::Rule(id) => uses.entry(*id).or_default().push(caller),
        Element::Seq(elements) | Element::Alt(elements) => {
            for inner in elements {
                count_in(inner, caller, uses);
            }
        }
        Element::Opt(inner) | Element::And(inner) | Element::Not(inner) => {
            count_in(inner, caller, uses);
        }
        Element::Rep { inner, .. } => count_in(inner, caller, uses),
        Element::Token(_) => {}
    }
}

/// Whether `id` is inlinable: exactly one reference, no annotations, not
/// the start rule.
fn is_candidate(grammar: &Grammar, id: RuleId, uses: &BTreeMap<RuleId, Vec<RuleId>>) -> bool {
    if id == grammar.start {
        return false;
    }
    let rule = grammar.rule(id);
    if rule.node
        || rule.pratt.is_some()
        || rule.list
        || rule.cold
        || rule.fragile
        || rule.wrap_error
        || !rule.sync.is_empty()
        || !rule.insert.is_empty()
    {
        return false;
    }
    uses.get(&id).is_some_and(|sites| sites.len() == 1)
}

/// Whether the reference closure of `from` (starting at its direct
/// references) hits any of `targets` — including `from` itself via a
/// cycle.
fn reaches(grammar: &Grammar, from: RuleId, targets: &BTreeSet<RuleId>) -> bool {
    let mut seen: BTreeSet<RuleId> = BTreeSet::new();
    let mut queue: VecDeque<RuleId> = grammar.rule(from).body.rule_refs().into_iter().collect();
    while let Some(rule) = queue.pop_front() {
        if targets.contains(&rule) {
            return true;
        }
        for next in grammar.rule(rule).body.rule_refs() {
            if seen.insert(next) {
                queue.push_back(next);
            }
        }
    }
    targets.contains(&from) && seen.contains(&from)
}

/// Replace every `Rule(id)` reference inside `element` with `body`.
fn replace_rule_ref(element: &Element, id: RuleId, body: &Element) -> Element {
    match element {
        Element::Rule(reference) if *reference == id => body.clone(),
        Element::Seq(elements) => Element::Seq(
            elements
                .iter()
                .map(|inner| replace_rule_ref(inner, id, body))
                .collect(),
        ),
        Element::Alt(elements) => Element::Alt(
            elements
                .iter()
                .map(|inner| replace_rule_ref(inner, id, body))
                .collect(),
        ),
        Element::Opt(inner) => Element::Opt(Box::new(replace_rule_ref(inner, id, body))),
        Element::And(inner) => Element::And(Box::new(replace_rule_ref(inner, id, body))),
        Element::Not(inner) => Element::Not(Box::new(replace_rule_ref(inner, id, body))),
        Element::Rep { inner, min, max } => Element::Rep {
            inner: Box::new(replace_rule_ref(inner, id, body)),
            min: *min,
            max: *max,
        },
        Element::Token(_) | Element::Rule(_) => element.clone(),
    }
}
