//! Left factoring with a fixpoint (issue 013): whenever two alternatives
//! of an `Alt` share a leading element sequence, the shared prefix is
//! hoisted behind a fresh helper rule — `A → α β | α γ` becomes
//! `A → α H` with `H → β | γ` — until no `Alt` in the grammar has a
//! common prefix left.
//!
//! Termination: every factoring step consumes at least one shared
//! leading element and moves the remainders into a fresh rule whose body
//! is strictly shorter than the pair it replaced; the total number of
//! shared prefixes is finite, so both the inner loop and the grammar-wide
//! fixpoint terminate.

use std::collections::BTreeSet;

use gir_ir::{Element, Grammar, Rule, RuleId};

/// Factor every common prefix in the grammar, to a fixpoint.
#[must_use]
pub fn left_factor(grammar: &Grammar) -> Grammar {
    let mut current = grammar.clone();
    loop {
        let next = one_factoring_pass(&current);
        if next == current {
            return next;
        }
        current = next;
    }
}

/// One pass: factor the first available common prefix anywhere.
fn one_factoring_pass(grammar: &Grammar) -> Grammar {
    let mut out = grammar.clone();
    let rule_count = out.rules.len();
    for idx in 0..rule_count {
        let id = RuleId(u32::try_from(idx).expect("rule count fits u32"));
        let body = out.rules[idx].body.clone();
        let factored = factor_element(&mut out, rule_name(grammar, id), &body);
        out.rules[idx].body = factored;
    }
    out
}

fn rule_name(grammar: &Grammar, id: RuleId) -> &str {
    grammar.rule_name(id)
}

/// Recursively factor one element tree, creating helper rules in
/// `grammar` as needed.
fn factor_element(grammar: &mut Grammar, base: &str, element: &Element) -> Element {
    match element {
        Element::Alt(branches) => {
            let mut factored: Vec<Element> = branches.clone();
            while let Some(step) = factor_alt_once(grammar, base, &factored) {
                factored = step;
            }
            let factored: Vec<Element> = factored
                .into_iter()
                .map(|branch| factor_element(grammar, base, &branch))
                .collect();
            if factored.len() == 1 {
                factored.into_iter().next().expect("one branch")
            } else {
                Element::Alt(factored)
            }
        }
        Element::Seq(elements) => Element::Seq(
            elements
                .iter()
                .map(|inner| factor_element(grammar, base, inner))
                .collect(),
        ),
        Element::Opt(inner) => Element::Opt(Box::new(factor_element(grammar, base, inner))),
        Element::And(inner) => Element::And(Box::new(factor_element(grammar, base, inner))),
        Element::Not(inner) => Element::Not(Box::new(factor_element(grammar, base, inner))),
        Element::Rep { inner, min, max } => Element::Rep {
            inner: Box::new(factor_element(grammar, base, inner)),
            min: *min,
            max: *max,
        },
        Element::Token(_) | Element::Rule(_) => element.clone(),
    }
}

/// One factoring step over an `Alt`: find the first maximal shared prefix
/// (deterministic scan order), hoist it behind a fresh helper rule.
/// Returns `None` when no pair of branches shares a prefix.
fn factor_alt_once(
    grammar: &mut Grammar,
    base: &str,
    branches: &[Element],
) -> Option<Vec<Element>> {
    let views: Vec<Vec<&Element>> = branches.iter().map(seq_view).collect();
    let mut best: Option<(usize, usize, usize)> = None; // (i, j, prefix len)
    for i in 0..branches.len() {
        for j in (i + 1)..branches.len() {
            let prefix = common_prefix(&views[i], &views[j]);
            if prefix == 0 {
                continue;
            }
            best = match best {
                // Strictly longer wins; ties keep the first pair scanned.
                Some((_, _, len)) if prefix <= len => best,
                _ => Some((i, j, prefix)),
            };
        }
    }
    let (i, _j, prefix) = best?;
    let shared: Vec<Element> = views[i][..prefix].iter().copied().cloned().collect();

    // Group every branch starting with exactly this prefix (stable).
    let mut group: Vec<usize> = Vec::new();
    for (idx, view) in views.iter().enumerate() {
        if view.len() >= prefix && view[..prefix] == views[i][..prefix] {
            group.push(idx);
        }
    }

    // Remainders (ε when the branch is exactly the prefix), deduplicated.
    let mut remainders: Vec<Element> = Vec::new();
    for &idx in &group {
        let rest: Vec<Element> = views[idx][prefix..].iter().copied().cloned().collect();
        let rest = if rest.is_empty() {
            Element::Seq(Vec::new())
        } else if rest.len() == 1 {
            rest.into_iter().next().expect("one element")
        } else {
            Element::Seq(rest)
        };
        if !remainders.contains(&rest) {
            remainders.push(rest);
        }
    }

    let helper_body = if remainders.len() == 1 {
        remainders.into_iter().next().expect("one remainder")
    } else {
        Element::Alt(remainders)
    };
    let helper = push_helper_rule(grammar, base, helper_body);

    let mut next: Vec<Element> = Vec::new();
    for (idx, branch) in branches.iter().enumerate() {
        if idx == group[0] {
            let mut combined = shared.clone();
            combined.push(Element::Rule(helper));
            next.push(Element::Seq(combined));
        } else if !group.contains(&idx) {
            next.push(branch.clone());
        }
    }
    Some(next)
}

/// Canonical view of a branch as a symbol sequence (`Seq` unwrapped).
fn seq_view(element: &Element) -> Vec<&Element> {
    match element {
        Element::Seq(elements) => elements.iter().collect(),
        other => vec![other],
    }
}

fn common_prefix(a: &[&Element], b: &[&Element]) -> usize {
    a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count()
}

/// Append a fresh helper rule with a collision-free name.
fn push_helper_rule(grammar: &mut Grammar, base: &str, body: Element) -> RuleId {
    let existing: BTreeSet<String> = grammar
        .rules
        .iter()
        .map(|rule| grammar.interner.lookup(rule.name).to_owned())
        .collect();
    let mut counter = 0;
    let mut name = format!("{base}_f{counter}");
    while existing.contains(name.as_str()) {
        counter += 1;
        name = format!("{base}_f{counter}");
    }
    let sym = grammar.interner.intern(&name);
    let id = RuleId(u32::try_from(grammar.rules.len()).expect("rule count fits u32"));
    grammar.rules.push(Rule {
        id,
        name: sym,
        body,
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
    id
}
