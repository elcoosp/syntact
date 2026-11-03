//! Pratt hint lowering (issue 013): rewrite a pratt-hinted rule's
//! left-recursive alternatives into the repetition shape the descent Pratt
//! backend executes, keeping the [`PrattHint`] annotation as the source of
//! binding powers and associativity at parse time.
//!
//! `expr → expr ⊕ tail | atom…` becomes `expr → atom (tail)*` — the
//! left recursion disappears without changing the language, and the hint
//! (`left_bp`/`right_bp`/`assoc`) decides grouping during parsing.

use gir_ir::{Element, Grammar, RuleId};

/// Lower every pratt-hinted rule with left-recursive alternatives into
/// `atom (tail)*` form.
#[must_use]
pub fn lower_pratt(grammar: &Grammar) -> Grammar {
    let mut out = grammar.clone();
    for rule in &mut out.rules {
        // The hint rides along unchanged; grouping (binding powers,
        // associativity) is a parse-time decision of the pratt backend.
        if rule.pratt.is_none() {
            continue;
        }
        let Element::Alt(branches) = rule.body.clone() else {
            continue;
        };
        let mut recursive_tails: Vec<Vec<Element>> = Vec::new();
        let mut atoms: Vec<Element> = Vec::new();
        for branch in &branches {
            match seq_leading_self(branch, rule.id) {
                Some(tail) => recursive_tails.push(tail.to_vec()),
                None => atoms.push(branch.clone()),
            }
        }
        // Guards: no left recursion to remove, or no base case (the
        // language would become empty) — leave the rule alone.
        if recursive_tails.is_empty() || atoms.is_empty() {
            continue;
        }
        let atom_alt = alt_or_single(atoms);
        let tail_alt = alt_or_single(recursive_tails.into_iter().map(Element::Seq).collect());
        rule.body = Element::Seq(vec![
            atom_alt,
            Element::Rep {
                inner: Box::new(tail_alt),
                min: 0,
                max: None,
            },
        ]);
    }
    out
}

/// The tail after a leading self-reference, or `None` when `branch` does
/// not start with `Rule(id)`.
fn seq_leading_self(branch: &Element, id: RuleId) -> Option<&[Element]> {
    match branch {
        Element::Seq(elements) => match elements.first() {
            Some(Element::Rule(first)) if *first == id => Some(&elements[1..]),
            _ => None,
        },
        Element::Rule(first) if *first == id => Some(&[]),
        _ => None,
    }
}

/// `Alt` over the list, unwrapped when there is exactly one element.
fn alt_or_single(elements: Vec<Element>) -> Element {
    if elements.len() == 1 {
        elements.into_iter().next().expect("one element")
    } else {
        Element::Alt(elements)
    }
}
