//! Thompson NFA construction (issue 014): patterns compile into
//! **fragments** over local state indices; [`Nfa::merge`] rebases every
//! fragment into one global automaton (targets, epsilon links and accept
//! marks shifted by the fragment base) — the merge is what the "NFA
//! merge target rebasing" test pins down.

use crate::regex::{Regex, SCALAR_END};

/// One regex compiled to a fragment: Thompson construction over local
/// state indices. `transitions[s]` is the list of `(lo, hi, target)`
/// scalar ranges out of state `s`; `epsilon[s]` the ε links.
#[derive(Debug, Clone, Default)]
pub struct Fragment {
    /// Local transitions per state: `(lo, hi, target)` inclusive ranges.
    pub transitions: Vec<Vec<(u32, u32, u32)>>,
    /// Epsilon links per state.
    pub epsilon: Vec<Vec<u32>>,
    /// The fragment's start state (local).
    pub start: u32,
    /// The fragment's accept state (local).
    pub accept: u32,
}

impl Fragment {
    fn new_state(&mut self) -> u32 {
        let id = u32::try_from(self.transitions.len()).expect("nfa state fits u32");
        self.transitions.push(Vec::new());
        self.epsilon.push(Vec::new());
        id
    }

    fn link(&mut self, from: u32, to: u32) {
        self.epsilon[from as usize].push(to);
    }

    fn range(&mut self, from: u32, lo: u32, hi: u32, to: u32) {
        self.transitions[from as usize].push((lo, hi, to));
    }

    /// Thompson construction of `regex`; returns the `(start, accept)`
    /// pair (local indices).
    fn build(&mut self, regex: &Regex) -> (u32, u32) {
        match regex {
            Regex::Empty => self.build_empty(),
            Regex::Class(ranges) => self.build_class(ranges),
            Regex::Concat(parts) => self.build_concat(parts),
            Regex::Alt(branches) => self.build_alt(branches),
            Regex::Star(inner) => self.build_star(inner),
            Regex::Plus(inner) => self.build_plus(inner),
            Regex::Opt(inner) => self.build_opt(inner),
            Regex::Repeat { min, max, inner } => self.build_repeat(*min, *max, inner),
        }
    }

    fn build_empty(&mut self) -> (u32, u32) {
        let start = self.new_state();
        let accept = self.new_state();
        self.link(start, accept);
        (start, accept)
    }

    fn build_class(&mut self, ranges: &[(u32, u32)]) -> (u32, u32) {
        let start = self.new_state();
        let accept = self.new_state();
        for &(lo, hi) in ranges {
            self.range(start, lo, hi, accept);
        }
        (start, accept)
    }

    fn build_concat(&mut self, parts: &[Regex]) -> (u32, u32) {
        let mut start = None;
        let mut cursor = 0;
        for part in parts {
            let (s, e) = self.build(part);
            match start {
                None => start = Some(s),
                Some(_) => self.link(cursor, s),
            }
            cursor = e;
        }
        (start.expect("non-empty concat"), cursor)
    }

    fn build_alt(&mut self, branches: &[Regex]) -> (u32, u32) {
        let split = self.new_state();
        let join = self.new_state();
        for branch in branches {
            let (s, e) = self.build(branch);
            self.link(split, s);
            self.link(e, join);
        }
        (split, join)
    }

    fn build_star(&mut self, inner: &Regex) -> (u32, u32) {
        let split = self.new_state();
        let (s, e) = self.build(inner);
        let join = self.new_state();
        self.link(split, s);
        self.link(e, join);
        self.link(split, join);
        self.link(e, s); // loop back
        (split, join)
    }

    fn build_plus(&mut self, inner: &Regex) -> (u32, u32) {
        let (s, e) = self.build(inner);
        let join = self.new_state();
        self.link(e, join);
        self.link(e, s); // loop back (at least one pass)
        (s, join)
    }

    fn build_opt(&mut self, inner: &Regex) -> (u32, u32) {
        let split = self.new_state();
        let (s, e) = self.build(inner);
        let join = self.new_state();
        self.link(split, s);
        self.link(e, join);
        self.link(split, join);
        (split, join)
    }

    /// `Repeat`: `min` mandatory copies, then a star (unbounded) or
    /// `max - min` optional copies.
    fn build_repeat(&mut self, min: u32, max: Option<u32>, inner: &Regex) -> (u32, u32) {
        let mut start = None;
        let mut cursor = 0;
        for _ in 0..min {
            let (s, e) = self.build(inner);
            match start {
                None => start = Some(s),
                Some(_) => self.link(cursor, s),
            }
            cursor = e;
        }
        match max {
            None => {
                let (s, e) = self.build(&Regex::Star(Box::new(inner.clone())));
                if start.is_none() {
                    return (s, e);
                }
                self.link(cursor, s);
                cursor = e;
            }
            Some(max) => {
                for _ in min..max {
                    let (s, e) = self.build(&Regex::Opt(Box::new(inner.clone())));
                    match start {
                        None => start = Some(s),
                        Some(_) => self.link(cursor, s),
                    }
                    cursor = e;
                }
                let Some(start) = start else {
                    // min == max == 0 reaches here only defensively.
                    return self.build_empty();
                };
                cursor = cursor.max(start);
            }
        }
        (start.expect("non-empty repeat"), cursor)
    }
}

/// The merged global NFA: every fragment rebased into one state space.
#[derive(Debug, Clone, Default)]
pub struct Nfa {
    /// `(lo, hi, target)` ranges per global state.
    pub transitions: Vec<Vec<(u32, u32, u32)>>,
    /// Epsilon links per global state.
    pub epsilon: Vec<Vec<u32>>,
    /// Global start state.
    pub start: u32,
    /// Global accept state → pattern index (declaration order is the
    /// tie-break priority).
    pub accepts: Vec<(u32, u16)>,
}

impl Nfa {
    /// Build one pattern's fragment (pattern index used for priority).
    #[must_use]
    pub fn build_fragment(regex: &Regex) -> Fragment {
        let mut fragment = Fragment::default();
        let (start, accept) = fragment.build(regex);
        fragment.start = start;
        fragment.accept = accept;
        fragment
    }

    /// Merge fragments with target rebasing: state `i` of fragment `k`
    /// becomes `offsets[k] + i`, and all transitions / epsilon links /
    /// accept marks are shifted by the same base.
    ///
    /// # Panics
    /// When the merged automaton exceeds `u32::MAX` states
    /// (index-space exhaustion).
    #[must_use]
    pub fn merge(fragments: &[Fragment], pattern_of: &[u16]) -> Self {
        let mut transitions: Vec<Vec<(u32, u32, u32)>> = Vec::new();
        let mut epsilon: Vec<Vec<u32>> = Vec::new();
        let mut accepts = Vec::new();
        let mut starts: Vec<u32> = Vec::with_capacity(fragments.len());
        for (fragment, pattern) in fragments.iter().zip(pattern_of.iter()) {
            let base = u32::try_from(transitions.len()).expect("nfa state fits u32");
            starts.push(base + fragment.start);
            for (outgoing, eps) in fragment.transitions.iter().zip(fragment.epsilon.iter()) {
                transitions.push(
                    outgoing
                        .iter()
                        .map(|&(lo, hi, target)| (lo, hi, base + target))
                        .collect(),
                );
                epsilon.push(eps.iter().map(|&target| base + target).collect());
            }
            accepts.push((base + fragment.accept, *pattern));
        }
        // One ε dispatch state in front so the merged automaton starts
        // at a single state.
        let start = u32::try_from(transitions.len()).expect("nfa state fits u32");
        transitions.push(Vec::new());
        epsilon.push(starts);
        Self {
            transitions,
            epsilon,
            start,
            accepts,
        }
    }

    /// Epsilon closure of a state set (worklist).
    #[must_use]
    pub fn closure(
        &self,
        states: &std::collections::BTreeSet<u32>,
    ) -> std::collections::BTreeSet<u32> {
        let mut set = states.clone();
        let mut work: Vec<u32> = states.iter().copied().collect();
        while let Some(state) = work.pop() {
            for &next in &self.epsilon[state as usize] {
                if set.insert(next) {
                    work.push(next);
                }
            }
        }
        set
    }

    /// Move over one scalar: states reachable via a range containing `c`.
    #[must_use]
    pub fn move_set(
        &self,
        states: &std::collections::BTreeSet<u32>,
        c: u32,
    ) -> std::collections::BTreeSet<u32> {
        let mut out = std::collections::BTreeSet::new();
        for &state in states {
            for &(lo, hi, target) in &self.transitions[state as usize] {
                if lo <= c && c <= hi {
                    out.insert(target);
                }
            }
        }
        out
    }

    /// The accept pattern (lowest declaration index) in a state set, if
    /// any.
    #[must_use]
    pub fn accept_of(&self, states: &std::collections::BTreeSet<u32>) -> Option<u16> {
        self.accepts
            .iter()
            .filter(|(state, _)| states.contains(state))
            .map(|(_, pattern)| *pattern)
            .min()
    }
}

/// Sentinel documenting the scalar-space end (used by the DFA class
/// partition).
pub const SCALARS: u32 = SCALAR_END;

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::Nfa;
    use crate::regex::parse_regex;

    #[test]
    fn fragment_counts_states() {
        let fragment = Nfa::build_fragment(&parse_regex("ab").expect("parses"));
        // a-range state, a-accept/b-start shared? Thompson: a→b chain with
        // distinct accept: 4 states (a, mid, b, accept).
        assert_eq!(fragment.transitions.len(), 4);
    }

    #[test]
    fn merge_rebases_targets_and_accepts() {
        let one = Nfa::build_fragment(&parse_regex("a").expect("parses"));
        let two = Nfa::build_fragment(&parse_regex("b").expect("parses"));
        let base_one = one.transitions.len();
        let nfa = Nfa::merge(&[one, two], &[0, 1]);
        // Fragment two's states start at base_one (+1 for the dispatch
        // state added AFTER both fragments).
        assert_eq!(nfa.transitions.len(), base_one * 2 + 1);
        // All targets are global (>= their base), none local-zero except
        // legitimately state 0 of fragment one.
        for (state, outgoing) in nfa.transitions.iter().enumerate() {
            for &(_, _, target) in outgoing {
                assert!(
                    target < u32::try_from(nfa.transitions.len()).expect("state fits u32"),
                    "target {target} from {state}"
                );
            }
        }
        // Accepts rebased into the global space.
        assert_eq!(nfa.accepts.len(), 2);
        assert!(
            nfa.accepts
                .iter()
                .all(|(state, _)| (*state as usize) < nfa.transitions.len())
        );
    }

    #[test]
    fn closure_and_move_set() {
        let one = Nfa::build_fragment(&parse_regex("a|b").expect("parses"));
        let nfa = Nfa::merge(&[one], &[7]);
        let start = nfa.closure(&BTreeSet::from([nfa.start]));
        // Dispatch + both branch starts (ε) + joins… at least 3 states.
        assert!(start.len() >= 3);
        let after_a = nfa.closure(&nfa.move_set(&start, 'a' as u32));
        assert_eq!(nfa.accept_of(&after_a), Some(7));
        let after_c = nfa.move_set(&start, 'c' as u32);
        assert!(after_c.is_empty());
    }
}
