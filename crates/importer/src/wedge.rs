//! The conformance wedge: kind-multiset extraction and diff (issue 037).
//!
//! A tree-sitter grammar's node kinds (named rules, named tokens,
//! anonymous literals) form the reference set for an import. The wedge
//! extracts the same multiset from a converted [`GirDoc`] and diffs the
//! two — any missing or invented kind is a conformance bug the report
//! alone would not surface.

use std::collections::BTreeMap;

use gir_syntax::{GirDoc, ItemSurf, RuleKindSurf};

/// The kind multiset of a converted grammar: named rules and named
/// tokens (one entry each) plus anonymous literals (set semantics, as
/// tree-sitter's node-types lists each anonymous kind once).
#[must_use]
pub fn kind_multiset(doc: &GirDoc) -> BTreeMap<String, u32> {
    let mut out = BTreeMap::new();
    for rule in &doc.rules {
        if rule.node {
            *out.entry(rule.name.clone()).or_default() += 1;
        }
    }
    for token in &doc.tokens {
        if token.node {
            *out.entry(token.name.clone()).or_default() += 1;
        }
    }
    for lit in anonymous_literals(doc) {
        out.entry(lit.to_owned()).or_insert(1);
    }
    out
}

/// Anonymous literal texts across a document's `seq` bodies and `sep`
/// separators.
fn anonymous_literals(doc: &GirDoc) -> Vec<&str> {
    let mut out = Vec::new();
    for rule in &doc.rules {
        match &rule.kind {
            RuleKindSurf::Seq { items } => {
                for el in items {
                    if let ItemSurf::Lit { text } = &el.item {
                        out.push(text.as_str());
                    }
                }
            }
            RuleKindSurf::Sep(cfg) => out.push(cfg.separator.as_str()),
            RuleKindSurf::Alt { .. } => {}
        }
    }
    out
}

/// The difference between the expected (tree-sitter) and actual
/// (imported) kind multisets.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KindDiff {
    /// Kinds present in `expected` but not in `actual`.
    pub missing: BTreeMap<String, u32>,
    /// Kinds present in `actual` but not in `expected`.
    pub extra: BTreeMap<String, u32>,
}

/// Diff two kind multisets.
#[must_use]
pub fn diff_kinds(expected: &BTreeMap<String, u32>, actual: &BTreeMap<String, u32>) -> KindDiff {
    let mut missing = BTreeMap::new();
    let mut extra = BTreeMap::new();
    for (kind, count) in expected {
        let actual_count = actual.get(kind).copied().unwrap_or(0);
        if actual_count < *count {
            missing.insert(kind.clone(), count - actual_count);
        }
    }
    for (kind, count) in actual {
        let expected_count = expected.get(kind).copied().unwrap_or(0);
        if expected_count < *count {
            extra.insert(kind.clone(), count - expected_count);
        }
    }
    KindDiff { missing, extra }
}
