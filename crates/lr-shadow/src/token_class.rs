//! Token-class inference (roadmap §6.1 pass 7): group tokens never
//! distinguished in any parser state — the tree-sitter `ts_symbol_map`
//! idea, extended.
//!
//! The concrete predicate: two terminals share a class iff their action
//! columns over the **resolved** LR(1) table are identical — every state
//! shifts them to the same target, reduces the same production, or acts
//! on neither. Tokens with no reachable action at all (declared but
//! unreferenced — reserved keywords, lexer-only tokens) collapse into
//! one never-distinguished class; the pseudo-EOF sentinel is not a
//! token and never enters a class.
//!
//! The extension beyond tree-sitter's map: the grouping is *checked*,
//! not just computed — because same-class tokens are machine-
//! indistinguishable, mapping every feed token to its class
//! representative preserves the table-driven driver's verdict
//! (`driver::parse`), the property `tests/token_class.rs` pins
//! end-to-end. Class members keep their declaration order, classes are
//! ordered by representative (smallest token id), and the computation
//! is deterministic for a given `(automaton, grammar)` pair.

use std::collections::BTreeMap;

use gir_ir::{Grammar, TokenId};

use crate::automaton::{Action, Automaton};
use crate::lower::Look;

/// One action-column cell, keyed for equality: `(discriminant, payload)`
/// over [`Action`] (`Shift` → `(0, state)`, `Reduce` → `(1, production)`,
/// `Accept` → `(2, 0)`); `None` = the state has no action for the token.
fn cell_key(action: Option<Action>) -> Option<(u8, u32)> {
    action.map(|action| match action {
        Action::Shift(state) => (0, state.0),
        Action::Reduce(production) => (1, production),
        Action::Accept => (2, 0),
    })
}

/// The never-distinguished grouping of a grammar's token table over a
/// compiled shadow automaton (roadmap §6.1 pass 7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenClasses {
    /// One class per group, ordered by representative (the smallest
    /// token id); members keep declaration order.
    classes: Vec<Vec<TokenId>>,
    /// Token id → class index (the token table is dense).
    index: Vec<usize>,
}

impl TokenClasses {
    /// Compute the classes: one action-column signature per token over
    /// the automaton's resolved table, identical signatures grouped.
    /// Tokens absent from every state's actions (declared but never
    /// referenced by a production) share the all-empty signature.
    ///
    /// # Panics
    /// When a token id exceeds the grammar's token-table length (the
    /// table is dense by declaration, so this is an IR invariant break).
    #[must_use]
    pub fn compute(automaton: &Automaton, grammar: &Grammar) -> Self {
        let mut groups: BTreeMap<Vec<Option<(u8, u32)>>, Vec<TokenId>> = BTreeMap::new();
        for token in &grammar.tokens {
            let id = usize::from(token.id.0);
            assert!(id < grammar.tokens.len(), "token ids are dense");
            let mut column = Vec::with_capacity(automaton.states.len());
            for state in &automaton.states {
                column.push(cell_key(state.actions.get(&Look::token(token.id)).copied()));
            }
            groups.entry(column).or_default().push(token.id);
        }
        // Members were pushed in declaration order, so each class's
        // first element is its representative; classes sort by it.
        let mut classes: Vec<Vec<TokenId>> = groups.into_values().collect();
        classes.sort_by_key(|class| class[0]);
        let mut index = vec![0_usize; grammar.tokens.len()];
        for (class_index, class) in classes.iter().enumerate() {
            for token in class {
                index[usize::from(token.0)] = class_index;
            }
        }
        Self { classes, index }
    }

    /// The classes, ordered by representative; each class's members keep
    /// declaration order.
    #[must_use]
    pub fn classes(&self) -> &[Vec<TokenId>] {
        &self.classes
    }

    /// The class count (the partition size over the token table).
    #[must_use]
    pub fn len(&self) -> usize {
        self.classes.len()
    }

    /// Whether no tokens were grouped (an empty token table).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.classes.is_empty()
    }

    /// The class index holding `token`.
    #[must_use]
    pub fn class_of(&self, token: TokenId) -> Option<usize> {
        self.index.get(usize::from(token.0)).copied()
    }

    /// The class representative of `token` (the smallest member id).
    #[must_use]
    pub fn representative(&self, token: TokenId) -> Option<TokenId> {
        self.class_of(token)
            .map(|class_index| self.classes[class_index][0])
    }

    /// Tokens the grouping merged away (`token_count - len`): every
    /// member beyond a class's representative.
    #[must_use]
    pub fn merged(&self) -> usize {
        self.index.len() - self.classes.len()
    }

    /// The size of the token table the classes were computed over.
    #[must_use]
    pub fn token_count(&self) -> usize {
        self.index.len()
    }
}
